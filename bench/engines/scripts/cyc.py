#!/usr/bin/env python3
"""Cycles and instructions per byte of the engine bench, from /usr/bin/time -l (macOS).

  python3 -I bench/engines/scripts/cyc.py <bin>[,<bin>...] <reps> <rounds> <case>[,<case>...] [<engine>[,<engine>...]]

Each run is one process: `<bin> --loop <engine> <case> <corpus> <reps>`, which feeds the case <reps> times, each
into a fresh engine. Per round every (bin, engine) pair runs once per case, the order rotated by one each round so
drift from other load spreads over all pairs. Reported per pair: min and median cycles/B and instructions/B over the
rounds (bytes = case size x reps), IPC, median and best wall MiB/s, peak footprint; the load average is printed
before every round.

A bin may be written <label>=<path>. Engines default to wongtty,ghostty. With more than one bin, an engine whose
name starts with "ghostty" runs on the first bin only (the branch bins carry the same pinned Ghostty); write
<engine>@<n> to run it on bin n (1-based) instead, e.g. ghostty@2 for a bin built with Ghostty main.

The corpus directory is $CORPUS, by default <repo>/target/engine-corpus, written by corpus.mjs when missing.
"""
import os
import re
import statistics
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, '..', '..', '..'))


def counter(out, key):
    m = re.search(r'(\d+)\s+' + key, out)
    if not m:
        sys.exit(f'no "{key}" in /usr/bin/time -l output:\n{out[-2000:]}')
    return int(m.group(1))


def load():
    return ' '.join('%.2f' % x for x in os.getloadavg())


def main():
    if len(sys.argv) < 5:
        sys.exit(__doc__)
    bins = []
    for i, spec in enumerate(sys.argv[1].split(',')):
        label, _, path = spec.rpartition('=')
        bins.append((label or (os.path.basename(path) if i == 0 else f'{os.path.basename(path)}#{i + 1}'), path))
    reps, rounds = int(sys.argv[2]), int(sys.argv[3])
    cases = sys.argv[4].split(',')
    engines = (sys.argv[5] if len(sys.argv) > 5 else 'wongtty,ghostty').split(',')
    corpus = os.environ.get('CORPUS') or os.path.join(ROOT, 'target', 'engine-corpus')
    if not all(os.path.exists(os.path.join(corpus, f'{c}.vt')) for c in cases):
        subprocess.run(['node', os.path.join(ROOT, 'bench', 'engines', 'corpus.mjs'), corpus], check=True,
                       stdout=subprocess.DEVNULL)

    pairs = []
    for e in engines:
        name, _, at = e.partition('@')
        if at:
            pairs.append((int(at) - 1, name))
        elif name.startswith('ghostty') and len(bins) > 1:
            pairs.append((0, name))
        else:
            pairs.extend((b, name) for b in range(len(bins)))

    print(f'corpus {corpus} reps {reps} rounds {rounds}')
    print(f'load {load()}')
    res = {}
    for r in range(rounds):
        print(f'round {r + 1} load {load()}', flush=True)
        for c in cases:
            for k in range(len(pairs)):
                b, e = pairs[(k + r) % len(pairs)]
                p = subprocess.run(['/usr/bin/time', '-l', bins[b][1], '--loop', e, c, corpus, str(reps)],
                                   capture_output=True, text=True)
                out = p.stdout + p.stderr
                if p.returncode != 0:
                    sys.exit(f'{bins[b][1]} --loop {e} {c}: exit {p.returncode}\n{out[-2000:]}')
                wall = float(re.search(r'([\d.]+) MiB/s', p.stdout).group(1))
                res.setdefault((c, b, e), []).append(
                    (counter(out, 'cycles elapsed'), counter(out, 'instructions retired'), wall,
                     counter(out, 'peak memory footprint')))
    print(f'load {load()}')
    for c in cases:
        size = os.path.getsize(os.path.join(corpus, f'{c}.vt')) * reps
        for b, e in pairs:
            v = res[(c, b, e)]
            cy = sorted(x[0] / size for x in v)
            ins = sorted(x[1] / size for x in v)
            wall = sorted(x[2] for x in v)
            print(f'{c:17} {bins[b][0]:22} {e:21} cyc/B min {cy[0]:7.3f} med {statistics.median(cy):7.3f}'
                  f'  ins/B min {ins[0]:7.2f} med {statistics.median(ins):7.2f}'
                  f'  IPC {statistics.median(ins) / statistics.median(cy):4.2f}'
                  f'  wall MiB/s med {statistics.median(wall):6.1f} best {wall[-1]:6.1f}'
                  f'  peak fp {max(x[3] for x in v) / 1048576:.1f} MiB')


if __name__ == '__main__':
    main()
