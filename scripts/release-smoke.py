"""Production Linux smoke: AT-SPI, wtype, isolated fixture and source-hidden mount namespace.

No WebKit inspector, JavaScript injection, mocked picker or test IPC.
The harness changes only its own app processes, fixtures and output directory.
"""
import ctypes as c, time
P = c.c_void_p
I = c.c_int
S = c.c_char_p
lib = c.CDLL('libatspi.so.0')
glib = c.CDLL('libglib-2.0.so.0')

def bind(name, ret, args):
    f = getattr(lib, 'atspi_' + name)
    f.restype = ret
    f.argtypes = args
    return f
init = bind('init', I, [])
desktop = bind('get_desktop', P, [I])
count = bind('accessible_get_child_count', I, [P, P])
child = bind('accessible_get_child_at_index', P, [P, I, P])
name0 = bind('accessible_get_name', P, [P, P])
role0 = bind('accessible_get_role_name', P, [P, P])
pid = bind('accessible_get_process_id', c.c_uint, [P, P])
glib.g_free.argtypes = [P]
action = bind('accessible_get_action_iface', P, [P])
do = bind('action_do_action', I, [P, I, P])
edit = bind('accessible_get_editable_text_iface', P, [P])
settext = bind('editable_text_set_text_contents', I, [P, S, P])
textiface = bind('accessible_get_text_iface', P, [P])
get_text = bind('text_get_text', P, [P, I, I, P])
comp = bind('accessible_get_component_iface', P, [P])
focus0 = bind('component_grab_focus', I, [P, P])

def string(q):
    if not q:
        return ''
    t = c.string_at(q).decode(errors='replace')
    glib.g_free(q)
    return t
name = lambda p: string(name0(p, None))
role = lambda p: string(role0(p, None))

def walk(p, depth=0):
    yield p
    if depth > 25:
        return
    for i in range(max(0, count(p, None))):
        q = child(p, i, None)
        if q:
            yield from walk(q, depth + 1)

def app(pid0=None):
    d = desktop(0)
    for i in range(count(d, None)):
        p = child(d, i, None)
        if name(p) == 'git-poseidon' and (pid0 is None or pid(p, None) == pid0):
            return p
    raise RuntimeError('Application not found')

def find(label, root=None, role0=None):
    root = root or app()
    return next((p for p in walk(root) if name(p) == label and (role0 is None or role(p) == role0)), None)

def wait(label, timeout=15, root=None, role0=None):
    start = time.monotonic()
    while time.monotonic() - start < timeout:
        p = find(label, root, role0)
        if p:
            return p
        time.sleep(0.01)
    raise RuntimeError('Not found: ' + label)

def click(label, settle=0.15):
    p = until(lambda: next((q for q in walk(app()) if role(q) == 'page tab' and name(q).startswith(label)), None)) if label == 'Alterações locais' else wait(label)
    assert do(action(p), 0, None), label
    time.sleep(settle)

def fill(label, value):
    p = wait(label)
    assert focus0(comp(p), None), label
    hypr('hl.dsp.focus({window="pid:' + str(app_pid) + '"})')
    subprocess.run([str(args.wtype), '-s', '100', '-M', 'ctrl', '-k', 'a', '-m', 'ctrl', value], check=True)
    time.sleep(0.2)

def dump(p=None, level=0):
    p = p or app()
    print(' ' * level, role(p), repr(name(p)), flush=True)
    if level > 22:
        return
    for i in range(max(0, count(p, None))):
        q = child(p, i, None)
        if q:
            dump(q, level + 1)
init()
if __name__ == '__main__':
    import argparse, json, os, pathlib, subprocess, tempfile, signal, importlib.util
    parser = argparse.ArgumentParser()
    parser.add_argument('prefix', type=pathlib.Path, help='Installed usr prefix in an isolated pacman root')
    parser.add_argument('output', type=pathlib.Path)
    parser.add_argument('--appimage', type=pathlib.Path)
    parser.add_argument('--appimage-extract-and-run', action='store_true')
    parser.add_argument('--branding-sizes', action='store_true', help='Capture production history at five supported sizes')
    parser.add_argument('--wtype', type=pathlib.Path, default=pathlib.Path('/usr/bin/wtype'))
    args = parser.parse_args()
    args.prefix = args.prefix.resolve()
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    binary = args.prefix / 'bin/git-poseidon'
    root = pathlib.Path(tempfile.mkdtemp(prefix='poseidon-release-'))
    repo = root / 'repo'
    repo.mkdir()
    env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_') and k not in ('WEBKIT_INSPECTOR_HTTP_SERVER', 'GIO_LAUNCHED_DESKTOP_FILE', 'GIO_LAUNCHED_DESKTOP_FILE_PID', '__NV_DISABLE_EXPLICIT_SYNC')}
    env.update(HOME=str(root), XDG_CONFIG_HOME=str(root / 'config'), XDG_DATA_HOME=str(root / 'data'), XDG_CACHE_HOME=str(root / 'cache'), XDG_DATA_DIRS=str(args.prefix / 'share') + ':/usr/local/share:/usr/share', PATH=str(args.prefix / 'bin') + ':/usr/bin', NO_AT_BRIDGE='0', GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=str(root / 'no-global'))

    def git(*argv):
        return subprocess.check_output(['/usr/bin/git', '-C', str(repo), *argv], env=env, text=True).strip()
    git('init', '-q', '--initial-branch=main')
    git('config', 'user.name', 'Alpha Release Demo')
    git('config', 'user.email', 'alpha@example.invalid')
    git('config', 'commit.gpgsign', 'false')
    (repo / 'demo.txt').write_text('base\n')
    (repo / 'notes.txt').write_text('keep base\n')
    git('add', '-A')
    git('commit', '-qm', 'Base Alpha')
    for i in range(8):
        git('commit', '--allow-empty', '-qm', f'History Alpha {i}')
    base = git('rev-parse', 'HEAD')
    (repo / 'demo.txt').write_text('prepared version\n')
    (repo / 'notes.txt').write_text('keep unstaged\n')
    report = {'checks': {}, 'timings_ms': {}, 'memory': {}, 'method': 'Production package, AT-SPI UI actions, Git oracle, app-only screenshots. Accessibility enabled only for test process. Source hidden by mount namespace. No devtools or Vite.'}
    checks = report['checks']
    childproc = None
    app_pid = None

    def windows():
        return json.loads(subprocess.check_output(['hyprctl', 'clients', '-j']))

    def hypr(expression):
        v = subprocess.check_output(['hyprctl', 'dispatch', expression], text=True).strip()
        assert v == 'ok', v

    def until(fn, timeout=15):
        started = time.monotonic()
        while time.monotonic() - started < timeout:
            try:
                v = fn()
            except RuntimeError as error:
                if str(error) != 'Application not found':
                    raise
                v = None
            if v:
                return v
            time.sleep(0.01)
        raise RuntimeError('Timed out waiting for production state')

    def stop():
        global childproc, app_pid
        if childproc:
            try:
                os.killpg(childproc.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                childproc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(childproc.pid, signal.SIGKILL)
        if app_pid:
            try:
                os.kill(app_pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        childproc = None
        app_pid = None

    def launch(menu=False, workaround=True, image=None):
        global childproc, app_pid
        print('Launch', menu, workaround, image, flush=True)
        before = {w['pid'] for w in windows()}
        process_env = env.copy()
        if workaround:
            process_env['__NV_DISABLE_EXPLICIT_SYNC'] = '1'
        argv = ['gtk-launch', 'io.gitposeidon.desktop.desktop'] if menu else [str(image or binary)]
        if image and args.appimage_extract_and_run:
            argv.append('--appimage-extract-and-run')
        source = str(pathlib.Path(__file__).resolve().parents[1])
        command = ['bwrap', '--ro-bind', '/', '/', '--dev-bind', '/dev', '/dev', '--bind', '/tmp', '/tmp', '--bind', os.environ['XDG_RUNTIME_DIR'], os.environ['XDG_RUNTIME_DIR'], '--tmpfs', source, '--chdir', '/tmp', '--', *argv]
        started = time.monotonic()
        childproc = subprocess.Popen(command, cwd='/tmp', env=process_env, stdout=open(args.output / ('appimage.log' if image else 'production.log'), 'a'), stderr=subprocess.STDOUT, start_new_session=True)
        w = until(lambda: next((w for w in windows() if w['pid'] not in before and w['class'] == 'git-poseidon'), None))
        report.setdefault('backends', {})['appimage' if image else 'menu' if menu else 'terminal'] = {'xwayland': w.get('xwayland'), 'class': w['class']}
        app_pid = w['pid']
        p = until(lambda: app(app_pid) if any((x['pid'] == app_pid for x in windows())) else None)
        until(lambda: find('Abrir por caminho…', p))
        elapsed = round((time.monotonic() - started) * 1000, 1)
        selector = 'pid:' + str(app_pid)
        hypr('hl.dsp.window.float({action="enable",window=' + json.dumps(selector) + '})')
        hypr('hl.dsp.window.resize({x=1440,y=900,window=' + json.dumps(selector) + '})')
        return elapsed
    app_original = app

    def own_app(_pid=None):
        return app_original(app_pid)
    globals()['app'] = own_app

    def shot(name):
        w = next((w for w in windows() if w['pid'] == app_pid and w['title'] == 'Git-Poseidon'))
        selector = json.dumps('address:' + w['address'])
        hypr('hl.dsp.focus({window=' + selector + '})')
        # Keep the whole test window on the output; resizing can leave it offscreen.
        full = w['size'] == [1920, 1080]
        if full:
            hypr('hl.dsp.window.fullscreen({mode="fullscreen",action="set",window=' + selector + '})')
        else:
            hypr('hl.dsp.window.move({x=0,y=60,relative=false,window=' + selector + '})')
        time.sleep(0.6)
        w = next(w for w in windows() if w['pid'] == app_pid)
        report.setdefault('captures', {})[name] = {'at': w['at'], 'size': w['size']}
        subprocess.run(['grim', '-g', f"{w['at'][0]},{w['at'][1]} {w['size'][0]}x{w['size'][1]}", str(args.output / (name + '.png'))], check=True)
        if full:
            hypr('hl.dsp.window.fullscreen({mode="fullscreen",action="unset",window=' + selector + '})')

    def branding_sizes(label):
        if not args.branding_sizes:
            return
        for width, height in [(1280,720), (1366,768), (1440,900), (1920,1080), (1000,900)]:
            hypr('hl.dsp.window.resize({x=' + str(width) + ',y=' + str(height) + ',window="pid:' + str(app_pid) + '"})')
            time.sleep(0.5)
            shot(label + '-' + str(width) + 'x' + str(height))
        hypr('hl.dsp.window.resize({x=1440,y=900,window="pid:' + str(app_pid) + '"})')

    def open_path():
        start = time.monotonic()
        click('Abrir por caminho…')
        fill('Caminho do repositório', str(repo))
        query_start = time.monotonic()
        click('Abrir caminho', settle=0)
        until(lambda: any(('Base Alpha' in name(p) for p in walk(app()))))
        report['timings_ms'].setdefault('history_open_response', []).append(round((time.monotonic() - query_start) * 1000, 1))
        return round((time.monotonic() - start) * 1000, 1)

    def memory():
        spec = importlib.util.spec_from_file_location('poseidon_memory', str(pathlib.Path(__file__).with_name('measure-linux-memory.py')))
        m = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(m)
        samples = []
        for _ in range(5):
            samples.append(m.sample(app_pid))
            time.sleep(0.2)
        return {'rss_mib': m.distribution([v['sum_rss_mib'] for v in samples]), 'pss_mib': m.distribution([v['sum_pss_mib'] for v in samples]), 'note': samples[-1]['note'], 'samples': samples}
    try:
        try:
            report['timings_ms']['startup_without_workaround'] = launch(workaround=False)
            checks['wayland_without_workaround'] = True
        except Exception as error:
            checks['wayland_without_workaround'] = False
            report['wayland_error'] = str(error)
        finally:
            stop()
        report['timings_ms']['terminal_startup'] = launch()
        checks['production_without_source'] = True
        time.sleep(1)
        report['memory']['idle'] = memory()
        shot('01-installed-welcome')
        report['timings_ms']['open_path_ui_and_poll'] = open_path()
        checks['open_repository'] = True
        time.sleep(1)
        report['memory']['repository'] = memory()
        shot('02-installed-history')
        branding_sizes('installed-history')
        click('Alterações locais')
        wait('Preparar arquivo: demo.txt')
        f = next((p for p in walk(app()) if role(p) == 'button' and name(p).startswith('Modificado demo.txt')))
        assert do(action(f), 0, None)
        until(lambda: any((name(p).startswith('Linhas do diff,') for p in walk(app()))))
        checks['working_diff_loaded'] = True
        shot('03-installed-working-diff')
        click('Histórico')
        click('Preparar arquivo: demo.txt')
        wait('Confiar neste repositório?', role0='heading')
        click('Cancelar')
        assert git('diff', '--cached', '--name-only') == ''
        checks['trust_refusal_no_write'] = True
        click('Preparar arquivo: demo.txt')
        click('Confiar e atualizar lista')
        wait('Revogar')
        start = time.monotonic()
        click('Preparar arquivo: demo.txt', settle=0)
        until(lambda: git('diff', '--cached', '--name-only') == 'demo.txt')
        report['timings_ms']['stage_ui_and_poll'] = round((time.monotonic() - start) * 1000, 1)
        wait('Retirar do index: demo.txt')
        f = next((p for p in walk(app()) if role(p) == 'button' and name(p).startswith('Modificado demo.txt')))
        assert do(action(f), 0, None)
        until(lambda: any((name(p).startswith('Linhas do diff,') for p in walk(app()))))
        checks['staged_diff_loaded'] = True
        shot('04-installed-staged-diff')
        click('Histórico')
        start = time.monotonic()
        click('Retirar do index: demo.txt', settle=0)
        until(lambda: git('diff', '--cached', '--name-only') == '')
        report['timings_ms']['unstage_ui_and_poll'] = round((time.monotonic() - start) * 1000, 1)
        assert (repo / 'demo.txt').read_text() == 'prepared version\n'
        checks['unstage_preserves_worktree'] = True
        click('Preparar arquivo: demo.txt')
        wait('Retirar do index: demo.txt')
        (repo / 'demo.txt').write_text('prepared version\nextra unstaged\n')
        fill('Assunto', 'Commit Alpha instalado')
        click('Revisar commit')
        wait('Criar commit')
        shot('04-installed-confirmation')
        start = time.monotonic()
        click('Criar commit', settle=0)
        until(lambda: git('rev-parse', 'HEAD') != base)
        report['timings_ms']['commit_ui_and_poll'] = round((time.monotonic() - start) * 1000, 1)
        new_head = git('rev-parse', 'HEAD')
        assert git('show', 'HEAD:demo.txt') == 'prepared version'
        assert (repo / 'demo.txt').read_text() == 'prepared version\nextra unstaged\n'
        assert (repo / 'notes.txt').read_text() == 'keep unstaged\n'
        assert git('log', '-1', '--format=%s') == 'Commit Alpha instalado'
        checks['commit_index_only'] = True
        until(lambda: any(('Commit Alpha instalado' in name(p) for p in walk(app()))))
        report['timings_ms']['commit_to_graph_ui'] = round((time.monotonic() - start) * 1000, 1)
        checks['head_graph_refreshed'] = True
        shot('05-installed-created')
        click('Commit')
        wait('Commit Alpha instalado', role0='heading')
        history = next((p for p in walk(app()) if role(p) == 'toggle button' and 'Commit Alpha instalado' in name(p)))
        assert focus0(comp(history), None)
        start = time.monotonic()
        subprocess.run([str(args.wtype), '-k', 'End'], check=True)
        wait('Base Alpha', role0='heading')
        report['timings_ms']['graph_end_to_details'] = round((time.monotonic() - start) * 1000, 1)
        subprocess.run([str(args.wtype), '-k', 'Home'], check=True)
        wait('Commit Alpha instalado', role0='heading')
        checks['graph_keyboard_navigation'] = True
        trust = root / 'data/io.gitposeidon.desktop/trust.json'
        assert trust.exists()
        stored = trust.read_bytes()
        checks['trust_persisted'] = True
        stop()
        report['timings_ms']['menu_startup'] = launch(menu=True)
        checks['desktop_launch'] = True
        open_path()
        click('Alterações locais')
        wait('Revogar')
        assert trust.read_bytes() == stored
        checks['reopen_trust'] = True
        click('Revogar')
        wait('Preparar arquivo: demo.txt')
        assert trust.read_bytes() != stored
        checks['revoke_persisted'] = True
        shot('06-installed-reopened-revoked')
        stop()
        if args.appimage:
            report['timings_ms']['appimage_startup'] = launch(image=args.appimage.resolve())
            open_path()
            checks['appimage_open_repository'] = True
            shot('07-appimage-history')
            branding_sizes('appimage-history')
            click('Alterações locais')
            click('Preparar arquivo: demo.txt')
            click('Confiar e atualizar lista')
            wait('Revogar')
            click('Preparar arquivo: demo.txt')
            wait('Retirar do index: demo.txt')
            before = (repo / 'demo.txt').read_bytes()
            click('Retirar do index: demo.txt')
            wait('Preparar arquivo: demo.txt')
            assert (repo / 'demo.txt').read_bytes() == before
            click('Preparar arquivo: demo.txt')
            wait('Retirar do index: demo.txt')
            fill('Assunto', 'Commit Alpha AppImage')
            click('Revisar commit')
            wait('Criar commit')
            click('Criar commit')
            until(lambda: git('rev-parse', 'HEAD') != new_head)
            assert git('log', '-1', '--format=%s') == 'Commit Alpha AppImage'
            assert (repo / 'notes.txt').read_text() == 'keep unstaged\n'
            checks['appimage_stage_unstage_commit'] = True
            shot('08-appimage-created')
            report['memory']['appimage_repository'] = memory()
            stop()
        checks['pass'] = True
    except Exception as error:
        report['error'] = str(error)
        try:
            import contextlib
            with open(args.output / 'failure-tree.txt', 'w') as f, contextlib.redirect_stdout(f):
                dump()
            shot('failure')
        except Exception:
            pass
        raise
    finally:
        stop()
        report['fixture'] = str(root)
        report['appimage_mode'] = 'extract-and-run' if args.appimage_extract_and_run else 'FUSE'
        (args.output / 'release-smoke.json').write_text(json.dumps(report, indent=2) + '\n')
