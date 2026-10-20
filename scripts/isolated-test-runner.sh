#!/bin/sh
set -eu

case "$1" in
  */deps/*) ;;
  *) exec "$@" ;;
esac

# Do not mutate the parent environment: Cargo and rustup keep their own HOME.
POSEIDON_TEST_REAL_GIT=$(command -v git)
export POSEIDON_TEST_REAL_GIT
test_root=$(mktemp -d "${TMPDIR:-/tmp}/poseidon-test-env.XXXXXXXX")
trap 'rm -rf -- "$test_root"' EXIT HUP INT TERM
mkdir -p "$test_root/bin" "$test_root/home" "$test_root/xdg" "$test_root/templates"

# GitProcess intentionally removes inherited GIT_* variables in production.
# Apply isolation at the executable boundary, after that sanitization. This is
# a real Git, with only system/global config and init templates isolated; local
# configuration, filters and hooks under test retain their native semantics.
cat > "$test_root/bin/git" <<'SH'
#!/bin/sh
export GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_SYSTEM=/dev/null
export GIT_CONFIG_GLOBAL=/dev/null
exec "$POSEIDON_TEST_REAL_GIT" -c "init.templateDir=$POSEIDON_TEST_TEMPLATES" "$@"
SH
chmod 700 "$test_root/bin/git"
export POSEIDON_TEST_TEMPLATES="$test_root/templates"
export HOME="$test_root/home" USERPROFILE="$test_root/home"
export XDG_CONFIG_HOME="$test_root/xdg"
export PATH="$test_root/bin:$PATH"

# env -u cannot enumerate Git's variable family portably; use Python only for
# constructing the child's environment, never a process-global test mutation.
python3 - "$@" <<'PY'
import os
import subprocess
import sys

env = {k: v for k, v in os.environ.items() if not k.upper().startswith('GIT_')}
env.update(GIT_TERMINAL_PROMPT='0', LC_ALL='C', TZ='UTC')
sys.exit(subprocess.call(sys.argv[1:], env=env))
PY
