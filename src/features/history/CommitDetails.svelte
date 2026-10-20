<script lang="ts">
  import Icon from '../../lib/ui/Icon.svelte';
  import RefBadges from './RefBadges.svelte';
  import CommitFilesList from '../diff/CommitFiles.svelte';
  import type {
    Commit,
    CommitDetail,
    CommitFiles,
    CommitRef,
  } from '../../lib/ipc/bindings';
  let {
    selected,
    refs,
    detail,
    loading,
    error,
    onretry,
    files,
    filesLoading,
    filesError,
    selectedFile,
    onopenfile,
    onretryfiles,
  }: {
    selected: Commit | null;
    refs: CommitRef[];
    detail: CommitDetail | null;
    loading: boolean;
    error: string | null;
    onretry: () => void;
    files: CommitFiles | null;
    filesLoading: boolean;
    filesError: string | null;
    selectedFile: number | null;
    onopenfile: (index: number) => void;
    onretryfiles: () => void;
  } = $props();
  const formatter = new Intl.DateTimeFormat('pt-BR', {
    dateStyle: 'short',
    timeStyle: 'short',
  });
  function fullDate(value: string) {
    const parsed = new Date(value);
    return Number.isNaN(parsed.valueOf()) ? value : formatter.format(parsed);
  }
  function initials(name: string) {
    return (
      name
        .trim()
        .split(/\s+/)
        .slice(0, 2)
        .map((s) => s[0])
        .join('')
        .toUpperCase() || '?'
    );
  }
  const merge = $derived((selected?.parents.length ?? 0) > 1);
</script>

<section class="detail-panel" aria-labelledby="detail-title">
  {#if selected}<div class="commit-detail">
      <div class="detail-eyebrow">
        <span class:merge-kind={merge}
          >{merge
            ? `Merge · ${selected.parents.length} pais`
            : 'Commit selecionado'}</span
        ><code title={selected.oid}>{selected.oid.slice(0, 8)}</code>
      </div>
      <h3 id="detail-title">{selected.summary || '(sem mensagem)'}</h3>
      {#if refs.length}<div class="detail-refs"><RefBadges {refs} /></div>{/if}
      <div class="author-card">
        <span class="avatar" aria-hidden="true"
          >{initials(selected.authorName)}</span
        >
        <div>
          <strong>{selected.authorName}</strong><span
            ><time datetime={selected.authoredAt}
              >{fullDate(selected.authoredAt)}</time
            >{#if selected.authorEmail}{` · ${selected.authorEmail}`}{/if}</span
          >
        </div>
      </div>
      <dl class="facts">
        <div>
          <dt>{selected.parents.length > 1 ? 'Pais' : 'Pai'}</dt>
          <dd>
            {#each selected.parents as parent}<code title={parent}
                >{parent.slice(0, 8)}</code
              >{:else}<span class="muted">Commit inicial · sem pai</span>{/each}
          </dd>
        </div>
        {#if selected.committerName !== selected.authorName || selected.committedAt !== selected.authoredAt}<div
          >
            <dt>Committer</dt>
            <dd>
              {selected.committerName} · {fullDate(selected.committedAt)}
            </dd>
          </div>{/if}
      </dl>
      <h4 class="message-heading">Mensagem</h4>
      {#if loading}<p class="muted" role="status">
          Lendo mensagem do commit…
        </p>{:else if error}<p class="detail-error" role="alert">
          {error}
        </p>
        <button class="text-button" onclick={onretry}>Tentar novamente</button
        >{:else if detail}<pre class="commit-message">{detail.message ||
            '(sem mensagem)'}</pre>{/if}
      <CommitFilesList
        files={files?.oid === selected.oid ? files : null}
        loading={filesLoading}
        error={filesError}
        selectedIndex={selectedFile}
        onopen={onopenfile}
        onretry={onretryfiles}
      />
      <details class="technical">
        <summary>Dados técnicos</summary>
        <dl class="metadata">
          <div>
            <dt>OID completo</dt>
            <dd class="mono">{selected.oid}</dd>
          </div>
          <div>
            <dt>Árvore</dt>
            <dd class="mono">{selected.treeOid}</dd>
          </div>
          <div>
            <dt>{selected.parents.length > 1 ? 'Pais' : 'Pai'}</dt>
            <dd class="mono">
              {selected.parents.length
                ? selected.parents.join('\n')
                : 'Commit inicial · sem pai'}
            </dd>
          </div>
          <div>
            <dt>Autoria</dt>
            <dd>
              {fullDate(selected.authoredAt)}
            </dd>
          </div>
          <div>
            <dt>Committer</dt>
            <dd>
              {selected.committerName}{selected.committerEmail
                ? ` <${selected.committerEmail}>`
                : ''}
            </dd>
          </div>
          <div>
            <dt>Registrado em</dt>
            <dd>{fullDate(selected.committedAt)}</dd>
          </div>
        </dl>
      </details>
    </div>{:else}<div class="panel-empty">
      <Icon name="commit" size={32} /><strong>Conheça cada mudança</strong>
      <p>Selecione um commit para ver seus metadados e sua mensagem.</p>
    </div>{/if}
</section>
