# [M] Sparse FETCH ranges

## Goal

A FETCH's cost follows the groups it can return, not the span of its range.
A track whose sequences are sparse, or whose history is long gone, answers a
wide range without spinning a worker or probing upstream once per missing
sequence.

## Plan

The standalone FETCH walk asks `track::Consumer::fetch_group` for every
sequence from start to the newest group, stepping over each miss. With no
fetch handler, every miss resolves at once, so a range over millions of
missing sequences runs without yielding. With a handler, a relay sends one
upstream FETCH per missing sequence. Either way a single request from a peer
buys work proportional to the newest sequence number.

Seek the next group the track can serve instead of stepping by one. Where a
relay cannot know which groups its upstream still holds, decide what bound
or refusal is honest rather than probing blindly. Benchmark range span and
present-group count as separate axes.

## Related

- [Moxygen compatibility](/quest/m1/moxygen/README.md) - the line whose FETCH walk this bounds
- [Fetch without SUBSCRIBE](/quest/m1/ietf-fetch-only.md) - also changes how a relay reaches upstream for fetches
