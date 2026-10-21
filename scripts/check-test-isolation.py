"""Repeat real mutation tests under hostile *temporary* host Git configuration."""
import os
from pathlib import Path
import subprocess
import tempfile


with tempfile.TemporaryDirectory(prefix="poseidon-host-config-") as directory:
    root = Path(directory)
    home = root / "home"
    home.mkdir()
    xdg = root / "xdg"
    (xdg / "git").mkdir(parents=True)
    hooks = root / "hooks"
    hooks.mkdir()
    marker = root / "unauthorized-program-ran"
    program = root / "host-program"
    program.write_text(f'#!/bin/sh\ntouch "{marker}"\nexit 1\n')
    program.chmod(0o700)
    for hook in ("pre-commit", "post-index-change", "post-checkout"):
        (hooks / hook).symlink_to(program)
    config = (
        f'[filter "hostile"]\n clean = {program}\n required = true\n'
        f'[credential]\n helper = {program}\n'
        f'[core]\n hooksPath = {hooks}\n'
        '[commit]\n gpgSign = true\n'
    )
    (home / ".gitconfig").write_text(config)
    (xdg / "git/config").write_text(config)
    system = root / "system.config"
    system.write_text(config)
    env = dict(os.environ)
    # Keep toolchain/cache locations when changing only the child process HOME.
    env.setdefault("CARGO_HOME", str(Path.home() / ".cargo"))
    env.setdefault("RUSTUP_HOME", str(Path.home() / ".rustup"))
    env.update(
        HOME=str(home), USERPROFILE=str(home), XDG_CONFIG_HOME=str(xdg),
        GIT_CONFIG_SYSTEM=str(system), GIT_CONFIG_GLOBAL=str(home / ".gitconfig"),
        GIT_CONFIG_COUNT="1", GIT_CONFIG_KEY_0="core.hooksPath",
        GIT_CONFIG_VALUE_0=str(hooks), GIT_DIR=str(root / "nonexistent.git"),
        GIT_WORK_TREE=str(root / "nonexistent-worktree"),
    )
    for attempt in range(3):
        print(f"Hostile host configuration: mutation suite {attempt + 1}/3", flush=True)
        subprocess.run(
            ["cargo", "test", "--locked", "-p", "poseidon-core",
             "--test", "fixtures", "--test", "stage", "--test", "stage_race",
             "--test", "commit_creation", "--test", "commit_race", "--test", "branches",
             "--test", "stash", "--test", "stash_race", "--test", "execution_policy",
             "--test", "execution_policy_race"],
            env=env, check=True,
        )
        if marker.exists():
            raise RuntimeError("A host-configured program executed in a fixture")
