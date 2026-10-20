# CI: integração Git e confiança

Investigação do PR #1, em 20/10/2026, sobre a branch
`ci/full-integration-tests` (base `43291a7`).

## Falhas observadas

- [Run 39437687710](https://github.com/binexe1113/Git-Poseidon/actions/runs/39437687710):
  rustfmt recusou a expressão em `tests/stage.rs`. Corrigida com `cargo fmt --all`.
- [Run 39357569385](https://github.com/binexe1113/Git-Poseidon/actions/runs/39357569385):
  o teste de filtros recebeu `["lfs", "pointer"]`, e o teste de recriação de
  repositório encontrou confiança indevida. O runner reportou Git 2.55.0.
- A fixture isolava o Git usado na preparação, mas o serviço preservava a
  configuração global/sistema real. Esse comportamento do produto é intencional;
  variáveis `GIT_*` herdadas são removidas pelo backend. Portanto apenas definir
  `GIT_CONFIG_GLOBAL` no workflow não isolaria os subprocessos do serviço.
- Device/inode não distingue uma substituição quando o filesystem reutiliza o
  inode. A branch do PR já havia acrescentado birth time; o teste original
  continuou preservado e ganhou uma regressão de 24 recriações consecutivas.

## Isolamento Linux

`.cargo/config.toml` usa um runner para executáveis de testes em `deps/`. Ele
cria HOME/XDG temporários, remove variáveis `GIT_*` herdadas antes de iniciar o
teste e coloca um executável intermediário no PATH. Esse executável inicia o
Git real com configuração global/sistema vazia e templates de init vazios,
inclusive depois da sanitização do backend. Não modifica a configuração do
usuário, nem as regras do aplicativo compilado. Exemplos e o aplicativo passam
pelo runner sem alteração de ambiente. Requer `sh` e Python 3.

Os testes continuam usando configuração local, filtros e hooks reais. A fixture
cria explicitamente `.git/hooks`, sem depender dos templates do host. Um teste
confere configuração global/sistema vazia tanto no Git da fixture quanto no
runner de produção. O teste de filtro inclui um segundo driver local para
verificar deterministicamente a aceitação de drivers adicionais; verifica
ausência de execução antes da confiança, recusa da escrita, paridade com
`git add` depois da autorização e ausência de novas execuções após revogação.

`python3 scripts/check-test-isolation.py` repete três vezes as suítes de fixtures,
stage, commits, branches e corridas de escrita. O ambiente pai tem HOME/XDG,
configuração sistema/global, hooks, helper de credenciais, assinatura, filtros,
`GIT_CONFIG_COUNT`, `GIT_DIR` e `GIT_WORK_TREE` hostis, todos temporários.
Falhas propagam ao job; os programas hostis deixam um marcador se executados.

## Identidade: garantias e limites

No Unix, a chave permanece common-dir canônico + device + inode + birth time
em nanossegundos. Alterar arquivos dentro de `.git` não altera sua identidade.
Ausência de birth time ou data anterior ao epoch retorna erro: leituras ficam
sem confiança e concessões/escritas falham, inclusive no store em memória.
Não há fallback para caminho, inode sozinho, mtime ou ctime.

O JSON antigo continua legível. Grants Unix antigos com chave `dev:ino` não
correspondem à chave nova: exigem nova autorização explícita, sem migrar
automaticamente confiança nem permissões de hooks. Um teste carrega o formato
antigo com ambas as permissões, verifica recusa, nova concessão sem hooks,
persistência, revogação e troca apenas do birth time com o mesmo inode.

Birth time não está disponível em todos os filesystems/kernel/backends. Rust
usa `statx` no Linux e os campos de criação das outras plataformas; veja a
[documentação oficial de Metadata::created](https://doc.rust-lang.org/std/fs/struct.Metadata.html#method.created).
Nesses sistemas sem suporte, escritas permanecem indisponíveis; não se tenta
simular identidade persistente com um nonce copiável dentro do repositório.

Birth time não é um identificador criptográfico: depende da resolução e
estabilidade fornecidas pelo filesystem. Os testes não provam unicidade em
todo filesystem, snapshots/restores ou contra um agente que possa manipular
metadados e o trust store. Há ainda a janela residual já existente entre a
revalidação e o subprocesso Git; não foi introduzida uma transação de filesystem.

macOS usa também device/inode/birth time, mas não foi executado nesta revisão.
No Windows, o código existente usa caminho + creation time, sem file ID nativo;
não recebeu validação nem garantia equivalente ao Unix. Registros antigos
`created:...` podem continuar correspondendo ao mesmo diretório; registros
`path-only` não recebem confiança. O isolamento automático deste PR é Linux.

## Validação

Os comandos abaixo devem ser executados na raiz. A CI mantém toda a suíte de
integração, Clippy com warnings como erros e build desktop sem bundle:

```sh
cargo fmt --all -- --check
cargo test --locked -p poseidon-core
python3 scripts/check-test-isolation.py
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
npm ci
npm run check
npm run check:bindings
npm run check:release
npm run format:check
npm run build
npm run build:desktop -- -- --locked
```

Resultados locais finais (Arch Linux, Git 2.56.0):

| Verificação | Resultado |
| --- | --- |
| rustfmt, Rust 1.90.0 e 1.99.1 | Passou |
| Suíte completa, Rust 1.90.0 e 1.99.1 | 117 testes em cada execução: 24 unitários e 93 de integração; nenhum ignorado ou falhando |
| Clippy workspace/all-targets/all-features com `-D warnings`, ambas as versões Rust | Passou |
| Ambiente hostil, três repetições, ambas as versões Rust | 49 testes por repetição, 147 por versão; nenhum programa do host executado |
| Stage/confiança em ext4 e tmpfs | 12 testes em cada filesystem, incluindo 24 recriações por execução |
| `npm ci`, check, bindings, release, Prettier e build frontend | Passaram com Node 22.12.0; também passaram com o Node 26.12.0 do sistema |
| Build Tauri Linux release sem bundle | Passou com Rust 1.99.1/Node 26.12.0 e Rust 1.90.0/Node 22.12.0 |

Uma sonda adicional de 256 recriações de diretório observou **255 reutilizações
de device/inode no ext4**, com 256 identidades completas distintas. No tmpfs
foram 256 identidades distintas sem reutilização de inode nessa amostra. Isso
reproduz a causa do problema antigo no Linux local, sem depender de o runner
do GitHub reutilizar inodes na próxima execução.

As ferramentas fixadas foram instaladas em diretórios temporários, sem alterar
o Rust/Node padrão nem configurações globais. Não houve teste gráfico desktop,
screenshot ou benchmark de UI: a mudança trata CI/segurança e o build apenas
compila o aplicativo. Não foi executado Ubuntu, Windows ou macOS localmente.
O PR remoto continua no commit original com o run de formatação vermelho;
o resultado remoto da correção só pode ser confirmado depois de atualizar o PR.

## Atualizar o PR

Nenhum commit, push ou merge foi feito por esta investigação. Após revisar:

```sh
git diff
git add .cargo/config.toml .github/workflows/build.yml CI.md README.md \
  scripts/isolated-test-runner.sh scripts/check-test-isolation.py \
  crates/poseidon-core/src/trust.rs crates/poseidon-core/tests/stage.rs \
  crates/poseidon-core/tests/fixtures.rs crates/poseidon-core/tests/support/mod.rs
git commit -m "ci: isolate Git integration tests and cover trust identity regressions"
git push origin ci/full-integration-tests
gh pr checks 1 --watch
```
