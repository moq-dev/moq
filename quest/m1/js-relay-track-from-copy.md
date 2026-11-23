# [S] A JS relay answers TRACK from the copy it holds

## Goal

A JS relay (`js/net`) answers a downstream TRACK from the copy its session
already accepted for that track, as the Rust relay does, so the answer waits
on no other node while the copy's upstream is slow.

## Plan

Found in [moq-dev/moq#9444](https://github.com/moq-dev/moq/pull/9444): the
JS range-table chain tests could not hold the warm relay's whole upstream
link, since that also held the TRACK each new downstream subscription asks
for there. They hold only the open subscription's answers instead
(`MockTransport.holdOpen` with `holdUnis`).

- `js/net/src/lite/subscriber.ts` `ConsumeBroadcast` overrides
  `resolveTrackInfo` to open a TRACK stream upstream every time
  (`Subscriber.resolveTrackInfo`, then `#trackInfo`), even when the session
  holds an accepted copy of the track whose info is already known.
- Answer from that copy's info when the session holds one, and open TRACK
  upstream only for a track it has no copy of, matching the Rust relay.
- Test: in `js/net/src/range-table.test.ts`, a chain whose warm relay's
  upstream is held whole (`server.hold()`) still answers TRACK and the range
  that relay placed from its cache.

Public API: none. Wire: none.
