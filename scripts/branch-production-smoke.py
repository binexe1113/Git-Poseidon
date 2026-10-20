"""Branch happy-path through AT-SPI in a standard desktop build; disposable fixture only."""
import argparse
import hashlib
import importlib.util
import json
import os
import pathlib
import shutil
import signal
import subprocess
import tempfile
import time
import types

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('binary', type=pathlib.Path)
parser.add_argument('output', type=pathlib.Path)
parser.add_argument('--wtype', type=pathlib.Path, required=True)
args = parser.parse_args()
args.output = args.output.resolve()
args.output.mkdir(parents=True, exist_ok=True)
root = pathlib.Path(tempfile.mkdtemp(prefix='poseidon-branches-production-'))
repo = root / 'repository'
repo.mkdir()
binary = root / 'bin/git-poseidon'
binary.parent.mkdir()
shutil.copy2(args.binary.resolve(), binary)
env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_') and k not in ('WEBKIT_INSPECTOR_HTTP_SERVER', 'LD_PRELOAD', 'FAKEROOTKEY')}
env.update(HOME=str(root), XDG_CONFIG_HOME=str(root / 'config'),
           XDG_DATA_HOME=str(root / 'data'), XDG_CACHE_HOME=str(root / 'cache'),
           GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=str(root / 'no-global'),
           NO_AT_BRIDGE='0', __NV_DISABLE_EXPLICIT_SYNC='1')

def git(*argv):
    return subprocess.check_output(['/usr/bin/git', '-C', str(repo), *argv], env=env, text=True).strip()

git('init', '-q', '--initial-branch=main')
git('config', 'user.name', 'Branches Production Demo')
git('config', 'user.email', 'branches@example.invalid')
git('config', 'commit.gpgsign', 'false')
(repo / 'base.txt').write_text('base\n')
git('add', '-A')
git('commit', '-qm', 'Base das branches de produção')
oid = git('rev-parse', 'HEAD')
spec = importlib.util.spec_from_file_location('ui', pathlib.Path(__file__).with_name('release-smoke.py'))
ui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ui)

def until(fn, timeout=20):
    start = time.monotonic()
    while time.monotonic() - start < timeout:
        try:
            result = fn()
            if result:
                return result
        except RuntimeError:
            pass
        time.sleep(.03)
    raise RuntimeError('Production UI timeout')

def hypr(expression):
    assert subprocess.check_output(['hyprctl', 'dispatch', expression], text=True).strip() == 'ok'

ui.until = until
ui.hypr = hypr
ui.subprocess = subprocess
ui.args = types.SimpleNamespace(wtype=args.wtype.resolve())
report = {'checks': {}, 'method': 'Standard embedded production build; AT-SPI and keyboard, fresh XDG and owned fixture; no inspector, JS injection or mock picker. Process-only NVIDIA workaround. Not an installed package.'}
checks = report['checks']
with (args.output / 'production.log').open('w') as log:
    process = subprocess.Popen([str(binary)], cwd='/tmp', env=env, stdout=log,
                               stderr=subprocess.STDOUT, start_new_session=True)
    ui.app_pid = process.pid
    original_app = ui.app
    ui.app = lambda: original_app(process.pid)
    try:
        until(lambda: next((w for w in json.loads(subprocess.check_output(['hyprctl', 'clients', '-j'])) if w['pid'] == process.pid), None))
        hypr('hl.dsp.window.float({action="enable",window="pid:' + str(process.pid) + '"})')
        hypr('hl.dsp.window.resize({x=1440,y=900,window="pid:' + str(process.pid) + '"})')
        hypr('hl.dsp.window.move({x=0,y=60,relative=false,window="pid:' + str(process.pid) + '"})')
        until(lambda: ui.find('Abrir por caminho…'))
        def shot(name):
            hypr('hl.dsp.focus({window="pid:' + str(process.pid) + '"})')
            time.sleep(.5)
            w = next(w for w in json.loads(subprocess.check_output(['hyprctl', 'clients', '-j'])) if w['pid'] == process.pid)
            subprocess.run(['grim', '-g', f"{w['at'][0]},{w['at'][1]} {w['size'][0]}x{w['size'][1]}", str(args.output / (name + '.png'))], check=True)
        def finish():
            ui.wait('Fechar')
            time.sleep(.4)
            ui.click('Fechar')
        def choose(name, label):
            current = git('symbolic-ref', '--short', 'HEAD')
            ui.click('Branch ' + name + (', atual' if current == name else ''))
            ui.click(label)
            ui.click('Revisar operação')
        ui.click('Abrir por caminho…')
        ui.fill('Caminho do repositório', str(repo))
        ui.click('Abrir caminho')
        ui.wait('Branch main, atual')
        checks['open_path_ui'] = True
        ui.click('Criar branch')
        ui.fill('Nome da branch', 'standard')
        ui.click('Revisar operação')
        ui.click('Confiar neste repositório')
        ui.wait('Confirmar criação')
        shot('01-standard-create-confirmation')
        ui.click('Confirmar criação')
        finish()
        assert git('rev-parse', 'standard') == oid
        checks['create'] = True
        choose('standard', 'Trocar para standard')
        ui.click('Confirmar troca')
        finish()
        ui.wait('Branch standard, atual')
        assert git('symbolic-ref', '--short', 'HEAD') == 'standard'
        checks['switch_and_head'] = True
        shot('02-standard-switched')
        ui.click('Branch standard, atual')
        ui.click('Renomear standard…')
        ui.fill('Novo nome da branch', 'renamed')
        ui.click('Revisar operação')
        ui.click('Confirmar renomeação')
        finish()
        ui.wait('Branch renamed, atual')
        assert git('symbolic-ref', '--short', 'HEAD') == 'renamed'
        checks['rename_current'] = True
        choose('main', 'Trocar para main')
        ui.click('Confirmar troca')
        finish()
        choose('renamed', 'Excluir renamed…')
        ui.click('Confirmar exclusão')
        finish()
        assert subprocess.run(['/usr/bin/git', '-C', str(repo), 'show-ref', '--verify', 'refs/heads/renamed'], env=env, capture_output=True).returncode != 0
        assert git('rev-parse', 'HEAD') == oid
        assert (repo / 'base.txt').read_text() == 'base\n'
        checks['safe_delete_and_files_preserved'] = True
        shot('03-standard-final')
        measured = subprocess.check_output(['python3', str(pathlib.Path(__file__).with_name('measure-linux-memory.py')), str(process.pid), '--samples', '20', '--interval', '.25'], text=True)
        report['memory'] = json.loads(measured)
        report['binary_sha256'] = hashlib.sha256(binary.read_bytes()).hexdigest()
        checks['pass'] = True
        (args.output / 'production-smoke.json').write_text(json.dumps(report, indent=2) + '\n')
    except Exception:
        with (args.output / 'failure-tree.txt').open('w') as tree:
            import contextlib
            with contextlib.redirect_stdout(tree):
                ui.dump()
        raise
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=5)
print(json.dumps(checks))
