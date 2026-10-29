# [S] An aborted live copy doesn't hold a track's end open

## Goal

A track whose live copy of a group was cut or evicted, while a fetched copy
still serves that group to its end, settles its declared end when the fetched
copy finishes, and a later abort of the live copy never turns that clean end
into an error.

## Plan

Found while landing [moq-dev/moq#7053](https://github.com/moq-dev/moq/pull/7053)
(planned 2026-10-28). `TrackState::is_settled` (`rs/moq-net/src/model/`)
checks only each slot's own `slot.group`. When that group was aborted but a
fetched copy covers it, the end stays unsettled, and a later abort makes the
clean end an error.

Recommended (confirm in the PR): an aborted live copy counts as nothing in
flight, so `is_settled` judges the slot by whichever copy serves its end, the
same meaning `Handle::is_settled` and `Slot::cut()` already use. Write the
failing case first: cut or evict a slot's live group while a fetched copy
serves it, finish the copy, and check the track's end settles cleanly. Mirror
it in `js/net` if its track end has the same check.

Public API: none. Wire: none.

## Related

- [Drop set](/quest/m1/drop-set.md) - reworks the cut and drop records next to this check
