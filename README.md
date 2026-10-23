# Git-Poseidon

[![CI](https://github.com/binexe1113/Git-Poseidon/actions/workflows/build.yml/badge.svg?branch=main)](https://github.com/binexe1113/Git-Poseidon/actions/workflows/build.yml)

<img src="public/poseidon.svg" width="80" height="80" alt="Poseidon: coroa, barba e tridente" />

Cliente Git desktop experimental com Rust, Tauri 2 e Svelte/TypeScript.

## Recursos

- Histórico paginado com Commit Graph virtualizado, branches e tags.
- Diffs de commits e alterações locais; stage e unstage por arquivo.
- Criação de commits a partir do index revisado.
- Branches locais: criar, trocar com `git switch`, renomear e excluir branches integradas.
- Stashes em modo somente leitura: listar, inspecionar metadados e consultar diffs tracked/index/untracked.
- Worktrees existentes: listar, inspecionar estado e alterações locais, e abrir um worktree no aplicativo.
- Confiança explícita por repositório, autorização separada de hooks e verificação de seleções obsoletas, locks e worktrees.

A versão em desenvolvimento é **0.2.0-alpha**. Não há merge, rebase ou operações remotas. Linux foi validado; Windows e macOS ainda precisam de validação. O consumo de memória do WebKit continua em investigação.

Stash está disponível **somente para leitura** nesta versão. Criar, aplicar (inclusive `--index`) e excluir estão desabilitados na interface e recusados pelo backend antes de iniciar qualquer comando Git. A exclusão nativa por posição não garante a identidade da entrada sob concorrência externa, e a inspeção prévia de configuração não impede hooks/filtros/drivers introduzidos na última janela. As escritas só poderão voltar com garantias demonstradas para esses casos; nenhuma política parcial de desativação de hooks é apresentada como isolamento completo. A listagem pode mudar por ações externas; seleções obsoletas exigem nova inspeção.

Worktrees estão disponíveis **somente para navegação e leitura**. Abrir um worktree troca apenas a sessão exibida: nenhuma branch é trocada e nenhum arquivo é alterado. A seleção é conferida no backend, que recusa a abertura se o registro de worktrees mudou, se o diretório foi movido, removido ou substituído, ou se o worktree está incompleto. Criar, remover, mover, bloquear, desbloquear, podar e reparar worktrees não estão disponíveis. A confiança do repositório é compartilhada pelos worktrees, mas filtros configurados apenas em um worktree exigem nova revisão antes de escritas nele.

A inspeção de alterações locais não executa filtros externos, mesmo após confiar no repositório. Por isso, arquivos transformados por filtros podem aparecer modificados depois do preparo. As escritas usam configuração capturada pelo backend; novas definições de filtros exigem renovar a confiança, e mudanças no conteúdo dos hooks exigem nova autorização separada.

As escritas protegidas foram validadas no Linux, com refs no formato `files` e index sem split index; repositórios compartilhados entre usuários não são suportados. As escritas são recusadas nos demais sistemas até a implementação dos guards equivalentes. O contexto protegido de inspeção também requer Unix nesta implementação; esses fluxos ainda não estão disponíveis no Windows. Git LFS é recusado nos caminhos que o exigem, sem armazenar conteúdo bruto como substituto. Filtros e hooks que dependem da localização original da pasta Git ou do script devem ser usados no terminal: os programas autorizados executam em contexto Git temporário, sem sandbox nem isolamento de suas dependências e efeitos. A concorrência externa ainda pode causar falhas parciais; não há retry nem rollback automático.

## Desenvolvimento

Requer Git 2.49+, Rust 1.90+, Node.js 22.12+ e as [dependências de sistema do Tauri](https://v2.tauri.app/start/prerequisites/).

```bash
npm ci
npm run desktop
```

`npm run dev` inicia somente o frontend; as operações Git exigem o aplicativo desktop.

```bash
npm run check
npm run format:check
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
npm run test:git
npm run check:bindings
npm run check:release
npm run build:desktop -- -- --locked
```

O executável fica em `target/release/git-poseidon`. Os testes Git utilizam fixtures temporárias.

No Linux, os executáveis de testes Rust usam HOME/configuração Git isolados e
requerem Python 3. Veja [CI, confiança e reprodução das regressões](CI.md).

## Wayland e NVIDIA

Algumas combinações de compositor, driver NVIDIA e WebKitGTK apresentam erro de protocolo na inicialização. Quando necessário, aplique o workaround somente ao processo:

```bash
__NV_DISABLE_EXPLICIT_SYNC=1 npm run desktop
# Ou para o binário compilado:
__NV_DISABLE_EXPLICIT_SYNC=1 ./target/release/git-poseidon
```

## Licenciamento

Código e assets próprios sob a [licença MIT](LICENSE). Consulte [LICENSING.md](LICENSING.md) para informações sobre terceiros. Dependências mantêm suas respectivas licenças. Não há pacotes binários publicados neste repositório.
