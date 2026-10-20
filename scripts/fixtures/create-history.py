#!/usr/bin/env python3
"""Create disposable synthetic data BEFORE inspection; never accept an existing path.

No git commit/reset/checkout, network, hooks or personal Git configuration.
The returned repository is frozen for the subsequent read-only comparison.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


def fingerprint(repo):
    return {
        str(p.relative_to(repo)): hashlib.sha256(p.read_bytes()).hexdigest()
        for p in sorted(repo.rglob("*")) if p.is_file()
    }


def main():
    root = Path(tempfile.mkdtemp(prefix="poseidon-comparison-"))
    repo = root / "demo-history"
    repo.mkdir()
    clean_env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env = dict(clean_env, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null",
               GIT_TERMINAL_PROMPT="0")

    def git(*args, data=None):
        return subprocess.run(["git", "-C", str(repo), *args], env=env,
                              input=data, capture_output=True, check=True).stdout

    git("init", "--initial-branch=main")
    stream = bytearray()
    mark = 0

    def commit(branch, message, parents=()):
        nonlocal mark
        mark += 1
        text = (message + "\n\nDados sinteticos para inspecao visual.\n").encode()
        content = f"revision {mark}\n".encode()
        stream.extend(f"commit refs/heads/{branch}\nmark :{mark}\n".encode())
        stream.extend(f"committer Equipe Demo <> {1735732800 + mark * 3600} +0000\n".encode())
        stream.extend(f"data {len(text)}\n".encode() + text + b"\n")
        for i, parent in enumerate(parents):
            stream.extend(f"{'from' if i == 0 else 'merge'} :{parent}\n".encode())
        stream.extend(b"M 100644 inline demo.txt\n")
        stream.extend(f"data {len(content)}\n".encode() + content + b"\n\n")
        return mark

    tip = 0
    for i in range(100):
        tip = commit("main", f"Base {i + 1:03d}: refinar historico", (tip,) if tip else ())
    login1 = commit("feature/login", "Iniciar fluxo de login", (tip,))
    login2 = commit("feature/login", "Validar formulario de login", (login1,))
    tip = commit("main", "Documentar navegacao", (tip,))
    tip = commit("main", "Ajustar contraste do painel", (tip,))
    merge = commit("main", "Merge feature/login", (tip, login2))
    fix1 = commit("fix/layout", "Alinhar detalhes e historico", (merge,))
    fix2 = commit("fix/layout", "Reduzir espacamento das linhas", (fix1,))
    tip = commit("main", "Preparar release visual", (merge,))
    tip = commit("main", "Merge fix/layout", (tip, fix2))
    tip = commit("main", "Revisar estados de carregamento", (tip,))
    tip = commit("main", "Concluir roteiro de demonstracao", (tip,))
    stream.extend(f"reset refs/tags/v0.1.0\nfrom :{merge}\n\n".encode())
    stream.extend(f"reset refs/remotes/origin/main\nfrom :{merge}\n\n".encode())
    stream.extend(b"done\n")
    # fast-import's 'reset' record declares initial refs; it is not `git reset`.
    git("fast-import", "--quiet", "--done", data=bytes(stream))
    git("read-tree", "HEAD")
    git("checkout-index", "--all")
    # No remote URL: inspecting remote-tracking refs cannot fetch a personal host.
    baseline = fingerprint(repo)
    (root / "before.json").write_text(json.dumps(baseline, indent=2))
    manifest = {"root": str(root), "repository": str(repo), "commits": mark,
                "branches": ["main", "feature/login", "fix/layout"],
                "head": git("rev-parse", "HEAD").decode().strip(),
                "fingerprint_files": len(baseline)}
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2))
    # Preparation ends here. Do not allow either GUI to refresh the index or
    # create maintenance caches in the repository during visual inspection.
    for p in repo.rglob("*"):
        p.chmod(0o555 if p.is_dir() else 0o444)
    repo.chmod(0o555)
    print(json.dumps(manifest))


if __name__ == "__main__":
    main()
