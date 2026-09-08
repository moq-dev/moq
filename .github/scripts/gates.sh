#!/usr/bin/env bash
#
# The aggregate verdict for gates.yml: one stable result a branch ruleset can
# require, whatever the diff selected.
#
# A required check has to report on every pull request, including the docs-only
# ones no lane covers. A path-filtered workflow cannot: it never starts, so its
# context never appears and the merge waits forever. So every lane is a
# conditional job inside one workflow that always starts, and this decides.
#
# The point of the script rather than a `needs` list is that `skipped` is
# ambiguous. GitHub reports the same word whether a lane was irrelevant to the
# diff or was never given the chance to run, and only the selector knows which.
# So each lane is checked against what the selector asked for:
#
#   selected, success            pass
#   selected, anything else      fail: the lane the diff needed did not pass
#   not selected, skipped        pass: irrelevant to this diff
#   not selected, anything else  fail: the workflow and the impact map disagree
#
# The last two rules also make the wiring self-checking: a lane the impact map
# emits with no job behind it, or a job with no lane in front of it, fails here
# rather than passing silently for however long nobody notices.
#
# Reads `toJSON(needs)` from GATES_NEEDS: a map of job id to `{result, outputs}`,
# where the `select` job's outputs are the impact map.

set -euo pipefail

: "${GATES_NEEDS:?GATES_NEEDS must hold toJSON(needs)}"

needs="$GATES_NEEDS"

# Without the selector there is nothing to compare a lane against, and treating
# an absent map as "nothing was selected" would pass every lane by skipping it.
selector="$(jq -r '.select.result // "missing"' <<<"$needs")"
if [[ "$selector" != success ]]; then
    echo "gates: the selector did not succeed ($selector); no lane can be verified" >&2
    exit 1
fi

report="$(jq -r '
	. as $needs
	| ($needs.select.outputs // {}) as $map
	| ($map | keys) as $lanes
	| (($needs | keys) - ["select"]) as $jobs
	| (
	    ($jobs[] | {
	      name: .,
	      selected: ($map[.] // "no-lane"),
	      result: ($needs[.].result // "missing")
	    }),
	    (($lanes - $jobs)[] | { name: ., selected: $map[.], result: "no-job" })
	  )
	| "\(.name) \(.selected) \(.result)"
' <<<"$needs" | sort)"

if [[ -z "$report" ]]; then
    echo "gates: no lanes and no jobs; the impact map and the workflow are both empty" >&2
    exit 1
fi

status=0
while read -r lane selected result; do
    case "$selected/$result" in
        true/success | false/skipped)
            printf '  ok       %-12s selected=%s result=%s\n' "$lane" "$selected" "$result"
            ;;
        true/*)
            printf '  FAILED   %-12s selected=%s result=%s\n' "$lane" "$selected" "$result"
            status=1
            ;;
        *)
            printf '  MISWIRED %-12s selected=%s result=%s\n' "$lane" "$selected" "$result"
            status=1
            ;;
    esac
done <<<"$report"

if ((status)); then
    echo "gates: a selected lane did not pass, or the impact map and the workflow disagree" >&2
fi

exit "$status"
