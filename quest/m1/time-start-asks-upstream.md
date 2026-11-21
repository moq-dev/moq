# [L] A relay copy asks upstream for every time start it cannot prove

## Goal

A lite-07 relay copy places a reader's time start only where it can prove what upstream would answer that reader, as `AGENTS.md` requires of a cache. For any other time start, it updates its upstream subscription and waits for the answer.
Two cases fail today, on `main` and with [moq-dev/moq#9172](https://github.com/moq-dev/moq/pull/9172) alike:

- A reader whose time falls inside a range another reader already asks for adds no range of its own (`wire_ranges` folds it), so upstream never answers for its time. The copy infers a placement from the covering range's start (#9172), from an earlier range that started past the time ([moq-dev/moq#9174](https://github.com/moq-dev/moq/pull/9174)), or waits out the arrival grace.
- A range answer is judged under the `Subscriber Max Age` in effect when the publisher resolved it. A range never resolves again (draft: "A Range resolves once"). A reader wider than that budget, or capped below the live edge, is never answered for its own terms: an empty answer (Range Start 0) leaves it waiting out the grace, and a start (`Answer::At`) places its time for every reader, though the publisher may have started past a group it expired under the narrower budget.

Done when a viewer behind a relay reads the same groups for a time start as it would reading the publisher directly, with any budget, joining before or after a narrower viewer.

## Plan

- No new wire message. A copy adds a reader's `Time` range to its upstream subscription as a range of its own in SUBSCRIBE_UPDATE whenever it cannot prove that reader's placement: a time no answer covers for the reader's budget and coverage, including one folded into another range, or one answered under a narrower budget. A new Range ID resolves fresh under the budget in effect then. Groups the subscription already delivered or dropped stay as they are. A dropped group the reader still wants comes on a request of its own, the miss path #9172's release step already relies on.
- Record each range's answer under the narrowest budget asked since that range was added (today `SubStream::stale_budget` is one value for the whole subscription). Then a range re-asked after a widening holds for every reader no wider than the new budget, as the maintainer noted on #9172.
- Accepted (maintainer, [moq-dev/moq#9172](https://github.com/moq-dev/moq/pull/9172#issuecomment-7054040452)): one more wire range per distinct unprovable reader time. These ranges are bounded by the copy's readers and retired with them. They replace the folding that keeps the range list short today, and that folding is what leaves those times unanswered: a partial hit always asks upstream for the miss.
- Then remove the inference no answer proves: #9172's covering-range placement (`landing_from` with a covering start, `Answer::Empty` releasing the barrier), #9174's earlier-range `Before`, and the hole placement in `TrackState::containing_received` (and its js/net twin in `track.ts`), which places a time between held groups whenever a higher group is held, with no Previous Start link. With groups 10, 15, and 20 at the publisher, a copy holding only 10 and 20 resolves `Time(18)` to 10 where the publisher answers 15. Keep only what the copy proves from held groups (#8638's Previous Start placement).
- Mirror in js/net (`lite/subscriber.ts` `#declare` and its demand ranges), and run `just test interop --all`.
- Reproduce first with the fixture in `rs/moq-net/tests/integration/waiting_time_start.rs` (publisher, relay, viewer). One case: an unlimited-budget ordered reader asks `Time(10)..Time(20)` and `Time(66)..` while the publisher holds only group 66. A second: a narrow viewer joins first, then a wider one asks from a time whose group the narrow budget expired.
- [moq-dev/moq#9228](https://github.com/moq-dev/moq/pull/9228) made a time start an ordinary group fill with a `floor`, but a received copy never asks one: its readers' time ranges go upstream on the demand subscription, which is what this quest extends.
- [no-relative-anchors](/quest/m2/time-addressing/no-relative-anchors.md) needs the same ask-and-wait for a Min Age live join, which it resolves as a time start. Whichever lands first builds the mechanism.

## Related

- [no-relative-anchors](/quest/m2/time-addressing/no-relative-anchors.md) - needs the same ask-and-wait for a Min Age live join
