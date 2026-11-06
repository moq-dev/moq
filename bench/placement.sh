#!/usr/bin/env bash
# Temporary experiment for quest/m1/bench-ci.md: tell a build difference from run placement.
# Builds the bench targets twice, as an A/A paired run does (this tree, and a worktree in
# TMPDIR, each with its own target directory), copies each executable into several
# byte-identical files, and runs every case from every file in rotated rounds.
set -euo pipefail
ROOT=$(git rev-parse --show-toplevel)
OUT=${PLACEMENT_OUT:-$ROOT/target/placement}
COPIES=${PLACEMENT_COPIES:-4}
ROUNDS=${PLACEMENT_ROUNDS:-3}
rm -rf "$OUT"
mkdir -p "$OUT/bin"
WORKTREE=$(mktemp -d "${TMPDIR:-/tmp}/moq-bench-base.XXXXXX")
git worktree add --detach "$WORKTREE" HEAD >/dev/null
trap 'git worktree remove --force "$WORKTREE"' EXIT

# name<TAB>package dir<TAB>cargo args<TAB>case filter
TARGETS=(
    $'wire\trs/moq-quic\t-p moq-quic --bench wire --features fuzz\twire_decode/padded/packets_(64|1024)$'
    $'producer_tick\trs/moq-stats\t-p moq-stats --bench producer_tick --features bench\tstats/tick/.*(_10pct|_100pct)'
)

build() {
    local side=$1 checkout=$2 name args
    for line in "${TARGETS[@]}"; do
        IFS=$'\t' read -r name _ args _ <<<"$line"
        # shellcheck disable=SC2086
        (cd "$checkout" && CARGO_TARGET_DIR=$OUT/target-$side cargo bench --locked --no-run --message-format=json-render-diagnostics $args) |
            jq -r 'select(.reason == "compiler-artifact" and .executable != null and any(.target.kind[]; . == "bench")) | .executable' >"$OUT/bin/$name-$side.path"
    done
}
build a "$ROOT"
build b "$WORKTREE"

FILES=()
for line in "${TARGETS[@]}"; do
    IFS=$'\t' read -r name _ _ _ <<<"$line"
    for side in a b; do
        src=$(cat "$OUT/bin/$name-$side.path")
        ln -s "$src" "$OUT/bin/$name-${side}0"
        for ((copy = 1; copy < COPIES; copy++)); do cp "$src" "$OUT/bin/$name-$side$copy"; done
    done
    sha256sum "$(cat "$OUT/bin/$name-a.path")" "$(cat "$OUT/bin/$name-b.path")"
done

printf 'file\tround\tcase\tmedian_ns\n' >"$OUT/results.tsv"
for line in "${TARGETS[@]}"; do
    IFS=$'\t' read -r name package _ filter <<<"$line"
    labels=()
    for side in a b; do for ((copy = 0; copy < COPIES; copy++)); do labels+=("$side$copy"); done; done
    for ((round = 0; round < ROUNDS; round++)); do
        for ((index = 0; index < ${#labels[@]}; index++)); do
            label=${labels[(index + round * 3) % ${#labels[@]}]}
            home=$OUT/criterion/$name/$label/round-$round
            (cd "$ROOT/$package" && CRITERION_HOME=$home "$OUT/bin/$name-$label" --bench \
                --warm-up-time 1 --measurement-time 2 --sample-size 30 --noplot "$filter" </dev/null >/dev/null)
            find "$home" -path '*/new/estimates.json' | while read -r estimates; do
                case=$(jq -r '.full_id // empty' "${estimates%estimates.json}benchmark.json")
                printf '%s\t%d\t%s\t%s\n' "$name-$label" "$round" "$case" "$(jq .median.point_estimate "$estimates")"
            done >>"$OUT/results.tsv"
        done
    done
done

python3 - "$OUT/results.tsv" <<'PY'
import csv, statistics, sys
from collections import defaultdict
rows = list(csv.DictReader(open(sys.argv[1]), delimiter="\t"))
by = defaultdict(lambda: defaultdict(list))
for row in rows:
    by[row["case"]][row["file"].rsplit("-", 1)[1]].append(float(row["median_ns"]))
for case, files in sorted(by.items()):
    ref = statistics.median(files["a0"])
    print(f"\n{case}  (a0 median {ref:.1f} ns; per-file rounds as % vs a0)")
    for label in sorted(files):
        values = files[label]
        print(f"  {label}: " + " ".join(f"{(v / ref - 1) * 100:+6.1f}" for v in values)
              + f"   median {(statistics.median(values) / ref - 1) * 100:+6.1f}")
PY
