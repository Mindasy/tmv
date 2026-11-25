#!/usr/bin/env python3
"""Exercise installed CLI against a real process, not only synthetic UI data."""
import json
import os
import pathlib
import subprocess
import sys
import tempfile

exe = str(pathlib.Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory(prefix='tmv-live-') as tmp:
    work = pathlib.Path(tmp)
    src = work / 'fixture.c'
    src.write_text('#include <unistd.h>\nint main(void) { sleep(60); return 0; }\n')
    target = work / 'fixture'
    subprocess.run(['cc', '-g', '-O0', '-fno-omit-frame-pointer', '-no-pie',
                    '-o', str(target), str(src)], check=True)
    proc = subprocess.Popen([str(target)])
    try:
        # Wait until exec has mapped the target.
        import time
        for _ in range(100):
            if str(target) in pathlib.Path(f'/proc/{proc.pid}/maps').read_text():
                break
            time.sleep(.05)
        def run(*args):
            return subprocess.run([exe, str(proc.pid), *args], check=True,
                                  text=True, capture_output=True, timeout=30).stdout
        snapshot = run('--json')
        json.loads(snapshot)
        (work / 'snapshot.json').write_text(snapshot)
        assert run('--report', '--top', '5')
        symbols = subprocess.check_output(['nm', '-n', str(target)], text=True)
        main = next(line.split()[0] for line in symbols.splitlines()
                    if line.split()[-1:] == ['main'])
        assert 'main' in run('--lookup', '0x' + main)
        assert run('--read', '0x' + main + ':32')
        assert run('--stack')
        for kind in ('treemap', 'flame'):
            svg = work / (kind + '.svg')
            run('--svg', str(svg), '--svg-format', kind)
            assert '<svg' in svg.read_text()
        subprocess.run([exe, '--replay', str(work / 'snapshot.json'), '--report'],
                       check=True, stdout=subprocess.DEVNULL, timeout=30)
        run('--baseline', str(work / 'snapshot.json'), '--report')
        os.kill(proc.pid, 0)
        print('Live layout passed: snapshot, report, lookup, memory read, thread unwind, SVG, replay, baseline')
    finally:
        proc.terminate()
        proc.wait(timeout=10)
