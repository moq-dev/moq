# [M] A relay copy asks upstream for every live join it cannot prove

## Goal

A lite-07 relay copy places a reader's live join (a range with no start and a Min Age) only where it can prove what upstream would answer that reader, as it now does for a time start ([moq-dev/moq#9307](https://github.com/moq-dev/moq/pull/9307)).
Today a join the copy can't prove from its held groups is inferred from the answers kept by Min Age (`TrackState::join_starts`, `feed_start`, js/net `#joinFed`).
That inference lost information each review round of [moq-dev/moq#9265](https://github.com/moq-dev/moq/pull/9265): an idle cache, an empty answer, and two bounded readers with the same Min Age whose group and empty answers collided.

Done when a viewer behind a relay joins at the same group as it would reading the publisher directly, for any Min Age and budget, joining before or after another viewer.

## Plan

- Decide how a copy asks for one reader's join. `Subscriber Min Age` is one value per subscription on the wire, so a per-reader join range can't carry its own Min Age. The options are a per-range Min Age (a lite-07 wire change, `-wip` only), or a time probe from the live edge the copy measures minus the Min Age, as #9307's probes do for a time start, which is only as fresh as the copy's own edge.
- Then remove `JoinStart`, its `Past` bound, and the `feed_start` selection (js/net `#joinFed`), which also settles the different covering answer Rust and JS pick today.
- Mirror in js/net and run `just test interop --all`.
