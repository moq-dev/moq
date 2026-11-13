#!/usr/bin/env bash
set -uo pipefail
dir=$1
total=0
: > "$dir/cpu"
while read -r d; do
  t=$( (nix log "$d" 2>/dev/null || true) | awk '/^MEASURE-TIMES/{f=1;next} f&&NF==2{print; n++} n==2{exit}' | python3 -c '
import sys,re
tot=0.0
for line in sys.stdin:
  for tok in line.split():
    m=re.match(r"(\d+)m([\d.]+)s",tok); tot+=int(m.group(1))*60+float(m.group(2))
print(f"{tot:.2f}")')
  echo "$t $d" >> "$dir/cpu"
done < "$dir/built"
python3 - "$dir" <<'PY'
import sys,os
d=sys.argv[1]
s,e=map(float,open(f"{d}/wall").read().split())
cpu=[l.split() for l in open(f"{d}/cpu")]
rust=[(float(c),p) for c,p in cpu if 'rust_' in p or 'moq-' in p]
tot=sum(float(c) for c,_ in cpu)
nomeasure=[p for c,p in cpu if float(c)==0 and ('rust_' in p or '-deps-' in p or 'moq-relay-0' in p or 'moq-cli-0' in p)]
subs=open(f"{d}/substituted").read().splitlines()
print(f"wall {e-s:.0f}s  builder CPU {tot:.0f}s  built {len(cpu)} drvs ({sum(1 for _,p in rust)} rust)  substituted {len(subs)} paths  load {open(f'{d}/load.before').read().split()[0]}->{open(f'{d}/load.after').read().split()[0]}")
if nomeasure: print("WARNING no MEASURE-TIMES for:", *nomeasure[:5])
PY
