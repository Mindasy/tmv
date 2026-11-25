#!/bin/sh
set -eu
cd "$(dirname "$0")/../.."
tmv=${TMV_BIN:-"$PWD/target/debug/tmv"}
library=${TMV_LIBRARY:-"$PWD/tools/heaptrace/libtmvtrace.so"}
if [ -z "${TMV_LIBRARY:-}" ]; then make -C tools/heaptrace; fi
cc -O0 -g -fno-builtin -Wl,--build-id -o tools/heaptrace/demo/memory_demo tools/heaptrace/demo/memory_demo.c
output=$(mktemp -d /tmp/tmv-session-test.XXXXXX)
build_id=$(readelf -n tools/heaptrace/demo/memory_demo | awk '/Build ID:/ {print $3; exit}')
python3 - "$build_id" "$output/rules.json" <<'PY'
import json,sys
json.dump([dict(label=label,build_id=sys.argv[1],function=fn) for label,fn in [('Cache','cache_create'),('Session.buffer','sessions_create'),('Temporary','temporary_work')]],open(sys.argv[2],'w'))
PY
"$tmv" run --library "$library" --no-ui --heap-rules "$output/rules.json" --output "$output/demo.tmvr" -- tools/heaptrace/demo/memory_demo
for at in 3 9 15; do
    "$tmv" report "$output/demo.tmvr" --at "$at" --json > "$output/$at.json"
done
"$tmv" report "$output/demo.tmvr" --at 9 --svg "$output/heap.svg"
"$tmv" report "$output/demo.tmvr" --at 9 --stacks > "$output/stacks.txt"
"$tmv" report "$output/demo.tmvr" --at 9 --analysis layout > "$output/layout.txt"
python3 - "$output" <<'PY'
import json,pathlib,sys
p=pathlib.Path(sys.argv[1])
for sec,expected in [(3,(32,8)),(9,(32,24)),(15,(0,24))]:
    f=json.load(open(p/f'{sec}.json'));totals={}
    for a in f['allocations']:totals[a['label']]=totals.get(a['label'],0)+a['size']
    assert totals.get('Cache',0)==expected[0]*1024**2,totals
    assert totals.get('Session.buffer',0)==expected[1]*1024**2,totals
    assert totals.get('Temporary',0)==0,totals
    assert f['snapshot'] and f['elapsed_ms']<=sec*1000
    for a in f['allocations']:
        if a['label'] not in ('Cache','Session.buffer'):continue
        symbols=[frame['symbol'] for frame in a['stack']]
        assert any('main+' in name for name in symbols),symbols
        if a['label']=='Cache':
            assert any('cache_create+' in name for name in symbols),symbols
            assert any('cache_initialize+' in name for name in symbols),symbols
        if a['label']=='Session.buffer' and sec>=9:
            assert any('sessions_grow+' in frame['symbol'] for frame in a['resize_stack']),a
            assert any('sessions_create+' in name for name in symbols),symbols
m=json.load(open(p/'demo.tmvr/manifest.json'))
assert m['state']=='complete',m
print('Session tests passed:',p)
PY
python3 tools/heaptrace/session-ui-test.py "$tmv" "$output/demo.tmvr"
python3 tools/heaptrace/run-ui-test.py "$tmv" "$library"
python3 tools/heaptrace/layout-mouse-test.py "$tmv"

if [ -n "${TMV_DEMO_DEST:-}" ]; then
    mkdir -p "$TMV_DEMO_DEST"
    cp -R "$output/demo.tmvr" "$TMV_DEMO_DEST/"
    cp "$output/rules.json" "$TMV_DEMO_DEST/rules.example.json"
fi
