# [M] Measure HTTP announcement snapshot fan-out

## Goal

Repeated announcement-list polling has measured cost proportional to useful
results, with avoidable copying or unrelated-route work removed if worthwhile.

## Plan

[serve_announced](/rs/moq-relay/src/web.rs) creates an announcement consumer,
collects prefixes, creates another Vec of strings and joins them per request.
Some output-size cost is necessary; repeated snapshot and
formatting work is the hypothesis, not proof of a bottleneck.

Sweep registered routes × HTTP pollers, independently varying matched prefix
size, route churn and path length. Compare an empty selective prefix, a small
result among many unrelated routes and the full listing. Measure CPU/request,
allocations/copied bytes, response latency and route-update latency while
polling. Exercise the real authorization/view path, and attribute its cost
separately from rendering.

Preserve admission, tenant isolation, hidden-prefix rules, response contents
and visibility during changes. Any shared snapshot must follow authorization
and content identity; avoid a global credential-blind cache. Prefer removing
intermediate allocations over a second route index unless measurements earn it.
This takes the recurring audit's HTTP announcement item; discovery wake/index
quests retain ownership of the underlying routing machinery.

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.
