# [M] A finished relay copy answers an empty SUBSCRIBE_START only when upstream would

## Goal

A lite-07 relay answers a range's `SUBSCRIBE_START` with no start (nothing to deliver at or
before its time) only when its upstream would answer the same, never because its copy finished
without placing the time from the groups it happens to hold.

## Plan

Found while fixing S1 of the subscribe progress fixes (moq-dev/moq#9564): a downstream copy now
records an open range's empty answer as "no group at or before the time", as it already did for
bounded ranges, so a wrong empty answer upstream now lands on every reader placing that time.

`TrackRun::report_started` in `rs/moq-net/src/lite/publisher.rs` answers `None if finished => None`
for a range whose key `resolved_keys` never resolved, once `Recv::Finished` or `unservable` says
nothing else arrives. First reproduce it: a relay copy that finished while holding only groups past
a range's time (history evicted or never fetched below), asked for that time. If it answers empty,
resolve the time the way an unfinished copy does (ask upstream, or answer from what the copy can
prove, per the root `AGENTS.md` cache rule) before answering. Check `js/net/src/lite/publisher.ts`
for the same path and fix both. If it does not reproduce, delete this quest and say why.

Compose with moq-dev/moq#9530, which changes how `TrackRun::resolve` walks below a start.
