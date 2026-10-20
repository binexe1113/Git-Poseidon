"""Profile a production Git-Poseidon binary using isolated, disposable XDG data."""
import argparse
import importlib.util
import json
import os
import pathlib
import signal
import subprocess
import tempfile
import time


def top_mappings(processes):
    result = []
    for process in processes:
        try:
            entries, entry = [], {}
            for line in pathlib.Path(f"/proc/{process['pid']}/smaps").read_text().splitlines():
                token = line.split()[0]
                if '-' in token and ':' not in token:
                    if entry:
                        entries.append(entry)
                    parts = line.split(maxsplit=5)
                    entry = {'mapping': parts[5] if len(parts) > 5 else '[anonymous]'}
                elif line.startswith(('Rss:', 'Pss:', 'Private_Dirty:', 'Shared_Clean:')):
                    entry[line.split(':')[0]] = int(line.split()[1])
            if entry:
                entries.append(entry)
            result.append({
                'name': process['name'], 'pid': process['pid'],
                'top_pss': sorted(entries, key=lambda v: v.get('Pss', 0), reverse=True)[:12],
            })
        except OSError:
            continue
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=pathlib.Path)
    parser.add_argument('output', type=pathlib.Path)
    parser.add_argument('--workaround', action='store_true')
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    spec = importlib.util.spec_from_file_location(
        'memory', pathlib.Path(__file__).with_name('measure-linux-memory.py'))
    memory = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(memory)
    with tempfile.TemporaryDirectory(prefix='poseidon-stabilization-memory-') as temporary:
        root = pathlib.Path(temporary)
        env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_') and k not in (
            'WEBKIT_INSPECTOR_HTTP_SERVER', 'LD_PRELOAD', 'FAKEROOTKEY', '__NV_DISABLE_EXPLICIT_SYNC')}
        env.update(HOME=str(root), XDG_DATA_HOME=str(root / 'data'),
                   XDG_CONFIG_HOME=str(root / 'config'), XDG_CACHE_HOME=str(root / 'cache'),
                   NO_AT_BRIDGE='1')
        if args.workaround:
            env['__NV_DISABLE_EXPLICIT_SYNC'] = '1'
        with (output / 'memory.log').open('w') as log:
            process = subprocess.Popen([str(args.binary.resolve())], cwd='/tmp', env=env,
                                       stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            try:
                time.sleep(3)
                samples = []
                for i in range(60):
                    if i:
                        time.sleep(.5)
                    samples.append(memory.sample(process.pid))
                pss = [s['sum_pss_mib'] for s in samples]
                report = {
                    'binary': str(args.binary.resolve()), 'workaround': args.workaround,
                    'method': 'Production binary; fresh isolated XDG, cwd /tmp, no inspector, '
                              'NO_AT_BRIDGE=1; 3s settling then 60 samples at 0.5s. '
                              'Source remains visible. GPU allocations excluded. Not a leak test.',
                    'samples': samples,
                    'rss_mib': memory.distribution([s['sum_rss_mib'] for s in samples]),
                    'pss_mib': memory.distribution(pss) if all(v is not None for v in pss) else None,
                    'top_mappings': top_mappings(samples[-1]['processes']),
                }
                (output / 'memory-idle.json').write_text(json.dumps(report, indent=2) + '\n')
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait()


if __name__ == '__main__':
    main()
