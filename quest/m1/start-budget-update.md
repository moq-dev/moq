# [M] Bind a resolved start to its subscriber budget

## Goal

A relay copy knows the subscriber `max_delay` used to select its upstream
start, so a later reader with a looser budget fills the history it can use.
The response proves the selecting budget even when SUBSCRIBE_UPDATE crosses
it in flight. Published wires retain their encodings and conservative
ambiguous-budget behavior.

## Plan

Decided 2026-10-29 (explicit implementation brief under `/afk`): adopt the
draft's recommended unpublished-wire scope, superseding the earlier
investigation-only brief. Lite-06's group-only response cannot prove which
update selected its start, so this cannot recover that history on a
published wire.

Implemented in [moq-dev/moq#7235](https://github.com/moq-dev/moq/pull/7235):
lite-07 START echoes Subscriber Max Age after its existing fields in Rust
and JS. The publisher captures it with the selected group, retaining both
while waiting for upstream START. Rust uses it in the existing budget-aware
fill ledger. Published responses omit it, and Rust conservatively records
the loosest sent budget when provenance is absent. No update revisions or
per-update STARTs are added; range responses and start bookkeeping retain
their own scope and branches.

Remaining: complete `just ci`, `just test interop --all`, and published-wire
compatibility validation; resolve relevant failures, then remove this quest
and mark the PR ready. The deterministic crossed-update tests, direct and
relayed intermediate-budget history-fill test, and exact published-wire
fixtures are implemented. Do not merge, enqueue, or watch this PR.

## Related

- [Drop set](/quest/m1/drop-set.md) - records the budget each drop was judged under
- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - owns the resolved-start response for ranges
- [Delete the old start bookkeeping](/quest/m1/subscribe-ranges/start-bookkeeping.md) - moves start state into the model and considers per-update responses
