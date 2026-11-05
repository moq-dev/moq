# [M] Flights: compressed groups and paced batch fills

## Goal

What [moq-dev/moq#7944](https://github.com/moq-dev/moq/pull/7944) left of
flights, once the maintainer settles its open decisions. A reader at a frame of
a compressed group that its copy shed is served by a flight, as a plain one is,
and fills that write a whole batch of groups at once stop evicting their own
earlier groups before the reader takes them.

## Plan

Landed in [moq-dev/moq#7944](https://github.com/moq-dev/moq/pull/7944): a
reader at a shed frame of a plain group on a track with a `track::Requests`
handler boards a flight. A flight is a separate copy asked of the handler from
that frame and paced by its fastest rider. The reader joins an existing copy
that holds its frame within 16 MB, less one log segment, of that copy's fastest
reader, and hands over to the copy ahead by the same rule. Each relay hop
serves a flight from its own flights, and a track without a handler keeps
refresh or `Evicted`.

Remaining, depending on the decisions under Waiting:

- Compressed groups fly nothing yet (`track::board` returns `None` for them).
  A flight that starts past frame 0 lacks the dictionary its plain riders
  inflate from.
- Fills that write a whole batch of groups at once evict their own earlier
  groups before the reader takes them (the follow-up from
  [moq-dev/moq#7339](https://github.com/moq-dev/moq/pull/7339), moved here on
  2026-11-02). This crosses groups, while a flight paces one group copy, so
  pacing the batch changes what `Fill::wanted` promises every handler.

## Waiting

- Maintainer: compressed flights. Options: (a) a compressed flight always starts
  at frame 0 and its riders skip to their frame, costing the prefix's transfer
  once per flight; (b) the flight carries the inflater's state from the cached
  copy, which needs the inflated copy to keep a snapshot per segment; (c)
  compressed groups keep refresh or `Evicted`. Recommendation: (a), since a
  compressed group's refresh already exists for joiners and a flight is the
  rarer path.
- Maintainer: paced batch fills. Options: (a) a quest of its own, where
  `Fill::wanted` yields the next group only once the readers took the one
  before; (b) here. Recommendation: (a), since it changes the handler
  contract and is not about flights.
