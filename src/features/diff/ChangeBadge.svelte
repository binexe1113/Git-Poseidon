<script lang="ts">
  import type { ChangeKind } from '../../lib/ipc/bindings';
  let { kind }: { kind: ChangeKind } = $props();
  const labels: Record<ChangeKind, [string, string]> = {
    added: ['A', 'Adicionado'],
    modified: ['M', 'Modificado'],
    deleted: ['D', 'Excluído'],
    renamed: ['R', 'Renomeado'],
    copied: ['C', 'Copiado'],
    typeChanged: ['T', 'Tipo alterado'],
    unmerged: ['U', 'Não mesclado'],
    untracked: ['N', 'Novo, não rastreado'],
    unknown: ['?', 'Desconhecido'],
  };
</script>

<span class="change-badge change-{kind}" title={labels[kind][1]}
  ><span aria-hidden="true">{labels[kind][0]}</span><span class="sr-only"
    >{labels[kind][1]}</span
  ></span
>
