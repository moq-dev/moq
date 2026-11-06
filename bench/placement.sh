#!/usr/bin/env bash
# Temporary experiment for quest/m1/bench-ci.md: does a process's string layout bias a case?
# Runs each case from one executable while padding CRITERION_HOME (copied to the heap by
# Criterion), or an environment variable no one reads (only the stack), by 0 to 15 bytes.
set -euo pipefail
ROOT=$(git rev-parse --show-toplevel)
OUT=${PLACEMENT_OUT:-$ROOT/target/placement}
ROUNDS=${PLACEMENT_ROUNDS:-2}
PADS=${PLACEMENT_PADS:-16}
rm -rf "$OUT"
mkdir -p "$OUT/bin"

# name<TAB>package dir<TAB>cargo args<TAB>case filter
TARGETS=(
    $'wire\trs/moq-quic\t-p moq-quic --bench wire --features fuzz\twire_decode/padded/packets_(64|1024)$'
    $'producer_tick\trs/moq-stats\t-p moq-stats --bench producer_tick --features bench\tstats/tick/(100|1000)p_(1|4)t_10pct'
)

for line in "${TARGETS[@]}"; do
    IFS=$'\t' read -r name _ args _ <<<"$line"
    # shellcheck disable=SC2086
    (cd "$ROOT" && CARGO_TARGET_DIR=$OUT/target cargo bench --locked --no-run --message-format=json-render-diagnostics $args) |
        jq -r 'select(.reason == "compiler-artifact" and .executable != null and any(.target.kind[]; . == "bench")) | .executable' >"$OUT/bin/$name.path"
done

printf 'variant\tround\tcase\tmedian_ns\n' >"$OUT/results.tsv"
for ((round = 0; round < ROUNDS; round++)); do
    for line in "${TARGETS[@]}"; do
        IFS=$'\t' read -r name package _ filter <<<"$line"
        for mode in home env; do
            for ((step = 0; step < PADS; step++)); do
                # Odd rounds walk the pads backwards, so drift over the run cannot pass for a pad effect.
                pad=$step
                if ((round % 2)); then pad=$((PADS - 1 - step)); fi
                padding=$(printf '%*s' "$pad" '' | tr ' ' x)
                home=$OUT/criterion/$name/$mode/round-$round/p$padding
                env_pad=
                if [[ $mode == env ]]; then
                    home=$OUT/criterion/$name/$mode/round-$round/p
                    env_pad=$padding
                fi
                (cd "$ROOT/$package" && PLACEMENT_PAD=$env_pad CRITERION_HOME=$home "$(cat "$OUT/bin/$name.path")" --bench \
                    --warm-up-time 1 --measurement-time 2 --sample-size 30 --noplot "$filter" </dev/null >/dev/null)
                find "$home" -path '*/new/estimates.json' | while read -r estimates; do
                    case=$(jq -r '.full_id' "${estimates%estimates.json}benchmark.json")
                    printf '%s%02d\t%d\t%s\t%s\n' "$mode" "$pad" "$round" "$case" "$(jq .median.point_estimate "$estimates")"
                done >>"$OUT/results.tsv"
            done
        done
    done
done

python3 - "$OUT/results.tsv" <<'PY'
import csv, statistics, sys
from collections import defaultdict
rows = list(csv.DictReader(open(sys.argv[1]), delimiter="\t"))
by = defaultdict(lambda: defaultdict(list))
for row in rows:
    by[row["case"]][row["variant"]].append(float(row["median_ns"]))
for case, variants in sorted(by.items()):
    ref = statistics.median(v for values in variants.values() for v in values)
    print(f"\n{case}  (overall median {ref:.1f} ns; each round as % vs it)")
    for variant in sorted(variants):
        print(f"  {variant}: " + " ".join(f"{(v / ref - 1) * 100:+6.1f}" for v in variants[variant]))
PY
