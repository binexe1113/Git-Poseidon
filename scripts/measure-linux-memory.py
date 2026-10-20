"""Read-only RSS/PSS sample for one application and its current descendants."""
import argparse
import os
import time
import json
import pathlib


def read_process(pid):
    fields = {}
    for line in (pathlib.Path('/proc') / str(pid) / 'status').read_text().splitlines():
        key, _, value = line.partition(':')
        fields[key] = value.strip()
    result = {
        'pid': pid,
        'ppid': int(fields['PPid']),
        'name': fields['Name'],
        'rss_kib': int(fields.get('VmRSS', '0 kB').split()[0]),
        'pss_kib': None,
    }
    stat = (pathlib.Path('/proc') / str(pid) / 'stat').read_text().rsplit(')', 1)[1].split()
    result['cpu_ticks'] = int(stat[11]) + int(stat[12])
    return result


def sample(root):
    if root <= 1:
        raise ValueError('Supply the Git-Poseidon process PID, greater than 1')
    processes = {}
    for path in pathlib.Path('/proc').iterdir():
        if path.name.isdigit():
            try:
                processes[int(path.name)] = read_process(int(path.name))
            except (OSError, KeyError, ValueError):
                continue
    if root not in processes or processes[root]['name'] != 'git-poseidon':
        raise ValueError('PID does not identify a running git-poseidon process')
    family = {root}
    while True:
        children = {pid for pid, data in processes.items() if data['ppid'] in family}
        if children <= family:
            break
        family |= children
    selected = [processes[pid] for pid in sorted(family)]
    # Read detailed memory accounting only for the requested application's family.
    for data in selected:
        try:
            for line in (pathlib.Path('/proc') / str(data['pid']) / 'smaps_rollup').read_text().splitlines():
                key, _, value = line.partition(':')
                if key in ('Rss', 'Pss', 'Pss_Anon', 'Pss_File', 'Pss_Shmem', 'Private_Clean', 'Private_Dirty', 'Shared_Clean', 'Shared_Dirty'):
                    data[('smaps_rss' if key == 'Rss' else key.lower()) + '_kib'] = int(value.split()[0])
        except (OSError, ValueError):
            pass
    pss_complete = all(data['pss_kib'] is not None for data in selected)
    return {
        'processes': selected,
        'sum_rss_mib': round(sum(data['rss_kib'] for data in selected) / 1024, 1),
        'sum_pss_mib': round(sum(data['pss_kib'] for data in selected) / 1024, 1) if pss_complete else None,
        'note': 'Single sample. RSS sums count shared pages repeatedly; PSS apportions them. Neither value includes GPU allocations.',
    }


def distribution(values):
    import math
    values = sorted(values)
    return {'samples': len(values), 'min': values[0], 'p50': values[math.ceil(len(values)*.5)-1], 'p95': values[math.ceil(len(values)*.95)-1], 'max': values[-1]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('pid', type=int)
    parser.add_argument('--samples', type=int, default=1)
    parser.add_argument('--interval', type=float, default=.25)
    args = parser.parse_args()
    if not 1 <= args.samples <= 10000 or args.interval <= 0:
        parser.error('samples must be 1..10000 and interval positive')
    samples = []
    for i in range(args.samples):
        if i:
            time.sleep(args.interval)
        entry = sample(args.pid)
        entry['monotonic'] = time.monotonic()
        samples.append(entry)
    if args.samples == 1:
        print(json.dumps(samples[0], indent=2))
        return
    first, last = samples[0], samples[-1]
    first_ticks = {p['pid']: p['cpu_ticks'] for p in first['processes']}
    last_ticks = {p['pid']: p['cpu_ticks'] for p in last['processes']}
    stable_family = first_ticks.keys() == last_ticks.keys() and all({p['pid'] for p in s['processes']} == first_ticks.keys() for s in samples)
    elapsed = last['monotonic'] - first['monotonic']
    cpu = 100 * sum(last_ticks[p] - first_ticks[p] for p in first_ticks) / os.sysconf('SC_CLK_TCK') / elapsed if stable_family else None
    pss = [s['sum_pss_mib'] for s in samples]
    print(json.dumps({'rss_mib': distribution([s['sum_rss_mib'] for s in samples]), 'pss_mib': distribution(pss) if all(v is not None for v in pss) else None, 'idle_cpu_percent_one_core': cpu, 'elapsed_seconds': elapsed, 'stable_process_family': stable_family, 'last_processes': last['processes'], 'note': 'Sum of app and descendants; RSS repeats shared pages. CPU=delta utime+stime / wall time, 100% is one core. No GPU memory. Build flags and inspector state must be recorded by the caller.'}, indent=2))


if __name__ == '__main__':
    main()
