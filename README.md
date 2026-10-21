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
- Confiança explícita por repositório, autorização separada de hooks e verificação de seleções obsoletas, locks e worktrees.

A versão em desenvolvimento é **0.2.0-alpha**. Não há merge, rebase ou operações remotas. Linux foi validado; Windows e macOS ainda precisam de validação. O consumo de memória do WebKit continua em investigação.

Stash está disponível **somente para leitura** nesta versão. Criar, aplicar (inclusive `--index`) e excluir estão desabilitados na interface e recusados pelo backend antes de iniciar qualquer comando Git. A exclusão nativa por posição não garante a identidade da entrada sob concorrência externa, e a inspeção prévia de configuração não impede hooks/filtros/drivers introduzidos na última janela. As escritas só poderão voltar com garantias demonstradas para esses casos; nenhuma política parcial de desativação de hooks é apresentada como isolamento completo. A listagem pode mudar por ações externas; seleções obsoletas exigem nova inspeção.

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
