# Changelog

## 0.2.0-alpha — em desenvolvimento

- Gerenciamento visual de branches locais: criar, trocar com git switch sem força, renomear e excluir branches integradas.
- Menus no painel/grafo, upstream informativo, revisão e autorização de hooks separada de commits.
- Revalidação de refs/HEAD/index/worktrees, recusa de colisões e reconciliação sem retry/rollback.
- Testes com fixtures temporárias e benchmarks em históricos de até 10 mil commits.

## 0.1.0-alpha — 2026-10-19 (candidata local, não publicada)

- MVP local: abrir repositórios existentes, consultar HEAD/branches/tags e histórico paginado com Commit Graph virtualizado.
- Diffs de commits, staged, unstaged e untracked, com limites de conteúdo, paginação e cancelamento de consultas superadas.
- Stage/unstage por arquivo, confiança explícita e revogável, proteção contra seleções obsoletas e respeito aos locks Git.
- Commits normais/iniciais usando o index revisado, composição/confirmacão, autorização separada de hooks e atualização do HEAD/grafo; alterações unstaged preservadas.
- Metadados Alpha, ícones próprios, pacote Arch x86_64 via makepkg e AppImage via Tauri. Assets de produção em `dist/frontend/`; artefatos em `dist/releases/v0.1.0-alpha/`.
- Abertura alternativa por caminho para ambientes cujo seletor GTK não conclui a seleção.

Experimental: sem operações remotas, checkout, gestão completa de branches, merge/rebase, resolução visual de conflitos ou assinatura GPG/SSH. Linux validado; Windows/macOS pendentes. Wayland/NVIDIA, RSS elevado, acessibilidade completa e rascunhos persistentes continuam limitados. Código próprio sob licença MIT; consulte LICENSING.md.
