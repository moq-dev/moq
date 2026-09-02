# [M] moqsrc: a rendition nobody answers keeps the session alive after the catalog closes

## Goal

A catalog that names a track its publisher never serves does not park a
subscriber: once the catalog closes, the subscription to that track resolves
with an error, `moqsrc` drops that one pump, and the session delivers its
terminal EOS when the served renditions drain.

## Plan

`moqsrc`'s pumps (`rs/moq-gst/src/source/imp.rs`) each await their own
subscription, and `follow_catalog` deliberately keeps them alive across
catalog close, because "has not taken a pad yet" also describes a pump spawned
microseconds ago; cancelling on close dropped whole broadcasts, which
`a_closing_catalog_keeps_the_renditions_it_named` guards against. So a pump for
a rendition nobody ever answers keeps the session open until the broadcast
ends or the element stops.

moq-net already fails fast for one case: `broadcast::Consumer::track` returns
`NotFound` when there is no producer and no `Dynamic`. The parking case is a
request that stays queued forever: a `track::Request` from `reserve_track`
that is never accepted, or a producer whose info never arrives, and over the
network every broadcast is a `Dynamic`, so that is the common shape.
A timeout would stand in for information the publisher has, so the fix is a
promise on the publisher side: closing the catalog means the announced set is
final.

- moq-net keeps its generic semantics: a dropped `track::Request` or a
  producer closed without a reason resolves pending subscribes with
  `Error::Dropped`, as `rs/moq-net/src/model/track.rs` documents, and a
  `request.reject(reason)` carries that reason. Verify and pin both; do not
  fold `NotFound` into the drop path, since a handler lost to a publisher or
  transport failure is not an absent track.
- hang publishers (`moqsink` reserves per pad, `moq-cli` and the FFI create
  tracks by name): when the catalog finishes, `reject(Error::NotFound)` every
  reserved or info-less track the catalog named, so their subscribers resolve
  and the catalog meaning stays out of moq-net.
- moqsrc: a pump whose subscribe resolves with an error ends that pump alone,
  and the loop exits once the catalog is closed and the served pumps drain.
- Tests: the existing test keeps its media; a new one names a reserved track,
  finishes the catalog, and asserts EOS within the served renditions' drain
  rather than at element stop.

## Closes

- [#3139](https://github.com/moq-dev/moq/issues/3139) - close this issue when the quest finishes
