# [M] A bounded reader waits for its edge before it fills a stale-dropped group

## Goal

A lite-07 reader whose ranges end below the live edge (`ranges: [{ end: 4n }]`) never fills a group the publisher dropped as stale under its own max age that it would convict itself once the groups above the drop arrive. What it delivers does not depend on whether the drop, the group streams, or the edge reaches the copy first. Rust and JS match.

## Plan

Found fixing the `js/net` test "a widening recovers the stale groups for a bounded lite-07 reader", which flaked on 218 of 480 runs under load: the test now waits for the copy to see group 4 (`reader.largest()`) before it reads. The product still races:

- A stale verdict does not settle a capped reader (`settles`, JS `track.ts`, Rust `track.rs`), since the upstream may have judged it against groups above the reader's cap. The reader judges the group itself, from the successors it holds.
- Group streams and the drop on the control stream are unordered. A drop that overtakes the groups leaves group 1 with no successor, so the reader judges it fresh and FETCHes it, delivering `[1, 3]` where `[3, 4]` follows once group 4 lands. Reading after the edge arrived judges it stale and asks nothing.
- Waiting for every owed key below the cap does not work: a track that never reaches the cap would owe forever, and a widening reader would never recover its group.

Decide the rule that makes the verdict checkable. Recommended: the copy records, with each stale drop, the highest key its upstream subscription reached (its cap), and a capped reader whose cap equals it treats the drop as settling, since the upstream judged it from the same groups. A reader with a different cap keeps asking for the group, as now. Alternatives: hold a non-settling run until the feed delivers one group above it (still racy between group 3 and 4), or accept the race and say so in `doc/`.

Public API: none. Wire: none.

## Waiting

- Maintainer: pick the rule that makes a stale verdict checkable by a capped reader: record the upstream's cap with each stale drop (recommended), hold the run until one group above it arrives, or accept the race; done once the Plan names it.

## Related

- [A widened resumed ordered reader asks its copies' stale give-ups again](/quest/m3/widened-front-reader-asks-stale-again.md) - the same stale-drop verdict across route copies
