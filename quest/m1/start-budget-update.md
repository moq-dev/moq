# [M] Bind a resolved start to its subscriber budget

## Goal

A relay copy knows the subscriber `max_delay` used to resolve its upstream
start, so a later reader with a looser budget fills the history it can use.
The response proves which budget was applied even when SUBSCRIBE_UPDATE
crosses it in flight. Published wires remain unchanged; this requires an
unpublished wire change rather than the originally planned lite-06 fix.

## Plan

Decided 2026-10-29 (explicit implementation brief under `/afk`): keep this
quest for the draft's recommended unpublished-wire scope. This supersedes
the earlier investigation-only brief. Echo the selecting Subscriber Max Age
in lite-07 START in Rust and JS, retaining it alongside a pending selected
group. Keep published encodings and their conservative ambiguous-budget
behavior. Do not add per-update STARTs or update revisions; coordinate with
[Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) and
[Delete the old start bookkeeping](/quest/m1/subscribe-ranges/start-bookkeeping.md)
without taking over their work. Advance the PR only, never merge or enqueue.

### Ordering evidence

Found by review of [moq-dev/moq#7168](https://github.com/moq-dev/moq/pull/7168):
the lite subscriber fixes `requested_max_delay` at SUBSCRIBE and passes it to
`start_resolved`. If the publisher applies a tightening before resolving the
start, a later reader between the two budgets gives up fillable history.

There is no lite-06 ordering that identifies the applied update:

- [`SubscribeStart`](https://github.com/moq-dev/moq/blob/0f68c6f9564e4dbfcd5b1f457eb3b2fdb420faa0/rs/moq-net/src/lite/subscribe.rs#L506)
  encodes only `group` on lite-06. `largest` is lite-07-only and carries no
  budget or request revision either. The
  [TypeScript codec](https://github.com/moq-dev/moq/blob/0f68c6f9564e4dbfcd5b1f457eb3b2fdb420faa0/js/net/src/lite/subscribe.ts#L611)
  has the same layout.
- [`SubscribeUpdate`](https://github.com/moq-dev/moq/blob/0f68c6f9564e4dbfcd5b1f457eb3b2fdb420faa0/rs/moq-net/src/lite/subscribe.rs#L581)
  carries replacement preferences, with no acknowledgment or revision.
  [The publisher applies them](https://github.com/moq-dev/moq/blob/0f68c6f9564e4dbfcd5b1f457eb3b2fdb420faa0/rs/moq-net/src/lite/publisher.rs#L3904)
  without sending a fresh START.
- The [release-branch draft](https://github.com/moq-dev/moq/blob/8677afaa6bc1420a235a249253dd56d09a378d2b/drafts/draft-lcurley-moq-lite.md#L1135)
  calls the same response SUBSCRIBE_OK and gives it only `Group`.
  Updates travel subscriber to publisher; the response travels publisher to
  subscriber. Ordering within each direction does not order the two directions.

For example, send SUBSCRIBE with 30 seconds, then UPDATE with 5 seconds,
then receive START at group 100. The publisher may have selected group 100
under 30 seconds before receiving the update, or selected it under 5 seconds
after applying the update. Both can produce the same received bytes: a start
does not prove that any earlier group was available under either budget.
Successful writes, flushes, transport acknowledgments, and local poll order
do not prove which preferences the publisher used to select it.

Recording the current 5-second budget can mislabel a start selected under
30 seconds. Recording the loosest possibly applied budget keeps 30 seconds,
so a later 15-second reader still skips the history this quest sought to
recover. The same ambiguity applies when a widening crosses START. Do not
ship either inference as a fix or claim the released-wire Goal is met.

### Implementation and validation

1. Define a response binding the resolved start to the budget used to select
   it on an unpublished wire. Prefer echoing that budget over keeping a
   history of sent updates solely to recover it. Coordinate the response
   shape and any per-update responses with the two quests above; sending a
   fresh START alone does not identify the applied budget.
2. Carry the budget with the selection through pending publisher state,
   rather than sampling the current budget when START is serialized. Record
   that proven budget in the relay's model. Implement Rust and TypeScript
   together, and update the lite draft and user-facing concepts as needed.
   Preserve published encodings and do not infer provenance on those wires.
3. Add deterministic direct and relayed integration coverage for a tightening
   applied before selection followed by a reader between the budgets, and
   for tightenings and widenings crossing an already-selected response.
   Verify which budget selected the start, not just when START arrived.
4. Run the affected checks, `just drafts check`, and `just test interop --all`,
   including published-wire compatibility, before completing the quest.

## Related

- [Drop set](/quest/m1/drop-set.md) - records the budget each drop was judged under, the same idea for drops
- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - owns the resolved-start response for ranges
- [Delete the old start bookkeeping](/quest/m1/subscribe-ranges/start-bookkeeping.md) - moves start state into the model and considers a response per update
