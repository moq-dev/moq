# [M] Resume identity retains only necessary state

## Goal

Long-lived browser fronts preserve track identity across routes without
unnecessarily retaining historical source object graphs.

## Plan

[Front metadata](/js/net/src/resume.ts) records the first `{info, source}` for
each track. A `Source` owns broadcast handles and a wire view; releasing its
last hold closes the cloned consumer but leaves those strong references.
This establishes a retained JS path, not that a closed socket or its native
buffers remain live. A fixed track set pins first-answer identities, not a
new source for every reconnect. Track-name churn plus route turnover is the
important adverse case.

Sweep fronts × distinct first-seen track names, independently varying route
replacements. Compare one stable track across many routes against new names
first answered by each route. Close readers after lookup, fetch and subscribe;
after cache expiry and browser GC, inspect heap dominators, retained source/
subscriber/transport objects and payload bytes. Measure transition CPU too.

Keep the metadata proof: the same name must still mean compatible content.
Preserve incompatibility refusal, concurrent cancellation and current-source
reuse. Coordinate with [route health](/quest/m2/route-health/README.md) before
changing identity representation; do not delete history just to reduce heap.

Follow the [survey benchmark contract](/quest/m2/performance-survey/README.md):
baseline first, paired evidence, equivalent work and nightly coverage. A
measured no-win is valid. No API or wire change is expected; report actual impact.
