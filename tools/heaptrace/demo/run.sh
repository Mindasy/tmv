#!/bin/sh
set -eu
cd "$(dirname "$0")/../../.."
root=$PWD
demo="$root/tools/heaptrace/demo"
tmv="$root/target/release/tmv"
test -x "$tmv"
test -f "$root/tools/heaptrace/libtmvtrace.so"
cc -O0 -g -Wall -Wextra -Werror -fno-builtin -Wl,--build-id -o "$demo/memory_demo" "$demo/memory_demo.c"
output=$(mktemp -d "$demo/run.XXXXXX")
build_id=$(readelf -n "$demo/memory_demo" | awk '/Build ID:/ {print $3; exit}')
python3 - "$build_id" "$output/rules.json" <<'PY'
import json,sys
assert sys.argv[1]
rules=[dict(label=label,build_id=sys.argv[1],function=fn) for label,fn in
       [('Cache','cache_create'),('Session.buffer','sessions_create'),('Temporary','temporary_work')]]
json.dump(rules,open(sys.argv[2],'w'),indent=2)
PY
TMV_TRACE="$output/trace" LD_PRELOAD="$root/tools/heaptrace/libtmvtrace.so" "$demo/memory_demo" > "$output/program.log" &
pid=$!
trap 'kill "$pid" 2>/dev/null || true' EXIT HUP INT TERM
trace="$output/trace.$pid.bin"
printf 'PID=%s\nOUTPUT=%s\n' "$pid" "$output"
for phase in 1 2 3; do
    attempts=0
    until grep -q "PHASE$phase" "$output/program.log"; do
        kill -0 "$pid"
        attempts=$((attempts+1))
        test "$attempts" -lt 300
        sleep 0.1
    done
    printf '\n--- PHASE %s ---\n' "$phase"
    "$tmv" "$pid" --heap-trace "$trace" --heap-rules "$output/rules.json" --report --top 8 | tee "$output/phase$phase.txt"
    "$tmv" "$pid" --heap-trace "$trace" --heap-rules "$output/rules.json" --json > "$output/phase$phase.json"
    "$tmv" "$pid" --heap-trace "$trace" --heap-rules "$output/rules.json" --svg "$output/phase$phase.svg"
    "$tmv" "$pid" --report --metric rss > "$output/phase$phase-rss.txt"
done
wait "$pid"
trap - EXIT HUP INT TERM
"$tmv" --heap-trace "$trace" --heap-rules "$output/rules.json" --json > "$output/final.json"
python3 - "$output" <<'PY'
import json,sys,pathlib
p=pathlib.Path(sys.argv[1])
for phase,expected in [(1,(32,8)),(2,(32,24)),(3,(0,24))]:
    r=json.load(open(p/f'phase{phase}.json'))
    totals={}
    for row in r['rows']:
        totals[row['label']]=totals.get(row['label'],0)+row['live_bytes']
    assert totals.get('Cache',0)==expected[0]*1024**2,(phase,totals)
    assert totals.get('Session.buffer',0)==expected[1]*1024**2,(phase,totals)
    assert totals.get('Temporary',0)==0,(phase,totals)
    temp=[r for r in r['rows'] if r['label']=='Temporary']
    assert sum(r['allocated_bytes'] for r in temp)==64*1024**2
r=json.load(open(p/'final.json'))
assert r['status']=='ended-at-collector-shutdown'
assert all(row['live_bytes']==0 for row in r['rows'] if row['label'] in ('Cache','Session.buffer','Temporary'))
print('\nPASS: all three phases and final release verified.')
print('Reports and SVGs:',p)
PY
