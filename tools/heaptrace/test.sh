#!/bin/sh
# Run on 64-bit Linux/glibc from the repository root after cargo build.
set -eu
tmv=${TMV_BIN:-"$PWD/target/debug/tmv"}
library=${TMV_LIBRARY:-"$PWD/tools/heaptrace/libtmvtrace.so"}
if [ -z "${TMV_LIBRARY:-}" ]; then make -C tools/heaptrace; fi
cc -O0 -g -fno-builtin -Wl,--build-id -o tools/heaptrace/fixture tools/heaptrace/fixture.c -pthread
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
TMV_TRACE="$tmp/trace" LD_PRELOAD="$library" tools/heaptrace/fixture
trace=$(echo "$tmp"/*.bin)
"$tmv" --heap-trace "$trace" --json > "$tmp/report.json"
python3 - "$tmp/report.json" "$tmp/rules.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r['status']=='ended-at-collector-shutdown',r
rows=[s for s in r['rows'] if 'retain_large' in s['site']]
assert len(rows)==1, r
assert rows[0]['live_bytes']==1024*1024,rows
assert rows[0]['live_count']==1,rows
assert all(s['live_bytes']==0 for s in r['rows'] if 'transient' in s['site'])
assert any('worker' in s['site'] for s in r['rows'])
assert all(s['live_bytes']==0 for s in r['rows'] if 'worker' in s['site'])
s=rows[0]
json.dump([dict(label='LargeBuffer',build_id=s['build_id'],function='retain_large',return_offset=s['return_offset'])],open(sys.argv[2],'w'))
PY
"$tmv" --heap-trace "$trace" --heap-rules "$tmp/rules.json" --json > "$tmp/named.json"
python3 - "$tmp/named.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert any(s['label']=='LargeBuffer' and s['live_bytes']==1048576 for s in r['rows'])
PY
"$tmv" --heap-trace "$trace" --heap-rules "$tmp/rules.json" --svg "$tmp/heap.svg"
test -s "$tmp/heap.svg"
echo 'heaptrace integration passed'
