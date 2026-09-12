# [S] moq-transport registry leftovers

## Goal

Three registered moq-transport values the tree still gets wrong after the
error-code sweep (#3531) and the joining FETCH fix (#3562): the
DEFAULT_PUBLISHER_PRIORITY property is neither written nor read, TRACK_STATUS
is dropped in Rust and answered with a success in JS instead of refused, and
two FETCH refusals lost their registered codes in the dev merge. Each is fixed
on every negotiated draft, with a byte-level test per version. TRACK_STATUS
itself stays unimplemented: answering it honestly needs a throwaway
SUBSCRIBE, which PR #3519 built and was closed for.

## Plan

### DEFAULT_PUBLISHER_PRIORITY (0x21)

The group header is done: dev stamps
`priority::to_wire(self.track.info().priority)`
(`rs/moq-net/src/ietf/publisher.rs:1873`) and js/net
`toWire(info.priority)` (`js/net/src/ietf/publisher.ts:239`). What remains
is the property. `rs/moq-net/src/ietf/properties.rs` knows only TIMESCALE
0x08 (`:27`) and DEFAULT_PUBLISHER_GROUP_ORDER 0x22 (`:34`), and
`js/net/src/ietf/properties.ts` the same two (`:8`, `:14`); 0x21 falls
through the unknown path. The subscriber builds `track::Info::default()`
with only timescale and max age (`rs/moq-net/src/ietf/subscriber.rs:1419`),
and an absent header priority flag resolves to a literal 128
(`rs/moq-net/src/ietf/group.rs:333`, `js/net/src/ietf/object.ts:254`).

- Add 0x21 beside 0x22 in both property modules. Encode it on SUBSCRIBE_OK
  and PUBLISH from `info.priority` through `priority::to_wire`; decode it
  through `priority::from_wire` into `track::Info::priority` where the
  subscriber builds its `Info`. The block is written from draft-17 on and
  read from draft-16 on, as `Properties::encode` already gates; draft-14 and
  15 carry the priority only in the group header.
- An absent header flag resolves to the track's declared default first and
  only then to the draft's fallback; confirm that fallback against each
  draft's text rather than keeping 128 by assumption, and cite the section
  in the type's docs. The model has no per-group priority, so a subgroup
  value that disagrees with the track's is decoded and dropped.
- Tests: 0x21 round-trips on SUBSCRIBE_OK on every draft that carries the
  block and is absent from the bytes on those that do not; a subgroup
  without the flag decodes to the declared default; a conflicting header
  leaves `track::Info::priority` unchanged.

### TRACK_STATUS refusal

`rs/moq-net/src/ietf/publisher.rs:439` matches `ietf::TrackStatus::ID` with
a warning and an empty future, so the request is never decoded and nothing
is written back; on draft-14 and 15 it rides a virtual stream over the
control stream whose reset is a no-op, so the peer waits out its timeout.
`request::Kind` (`rs/moq-net/src/ietf/error.rs:149`) has Subscribe, Fetch,
Publish, PublishNamespace, and SubscribeNamespace and no TrackStatus.
`runTrackStatusRequest` (`js/net/src/ietf/publisher.ts:1022`) answers
draft-15+ with a bare `RequestOk`, a success, and draft-14 with a
`TrackStatus` 0x0e body carrying the draft-13 status enum.

- Add `Kind::TrackStatus`. Decode the request so the stream is consumed,
  reply with the per-draft refusal (TRACK_STATUS_ERROR 0x0f on draft-14,
  REQUEST_ERROR from draft-15 on) carrying NOT_SUPPORTED through
  `request::to_code(&Error::Unsupported, Kind::TrackStatus, version)`, and
  close the writer explicitly, mirroring `run_publish_stream` in
  `subscriber.rs` (#3348).
- `runTrackStatusRequest` sends the same refusal on every draft instead of a
  success.
- `js/net/src/ietf/adapter.ts:638` routes message 0x0e to a
  SubscribeNamespace stream on every version, so a draft-14 TRACK_STATUS_OK
  would be handed to whatever SubscribeNamespace stream exists and read as
  a namespace. 0x0e is TRACK_STATUS_OK only on draft-14, absent on draft-15,
  and NAMESPACE_DONE from draft-16 on. Guard the case on the version (three
  lines) and add the adapter test from PR #3519: an unsolicited 0x0e on
  draft-14 refuses the session, matching Rust's `classify`.
- Tests: a byte-exact transport-log test per version, in both languages,
  that a TRACK_STATUS request yields the refusal and nothing else.

### FETCH refusal codes

#3562 on main sent INVALID_JOINING_REQUEST_ID (0x7 on draft-14, 0x32 after)
for a joining FETCH naming no subscription and INVALID_RANGE (0x5 on
draft-14, 0x11 after) for an empty snapshot. The dev merge lost both:
`reject_fetch` keys the code on an `Error` through `request::to_code`
(`rs/moq-net/src/ietf/error.rs:248`), which has no variant for either, so
the `None` arm (`publisher.rs:1057`, "no such subscription") and the
`Joined::Empty` arm (`:1080`, "no objects at subscription start") both pass
`Error::NotFound` and land on DOES_NOT_EXIST.

- Add the two registry entries to `request` with their per-draft values,
  and the `Error` variants they need (`Error` is `#[non_exhaustive]`,
  `rs/moq-net/src/error.rs:251`, so adding is not a break); `from_code`
  reads them back symmetrically, and `EVERY_ERROR` in the registry tests
  grows by two.
- Switch the publisher tests that assert `does_not_exist(version)`
  (`publisher.rs:2756`, used at `:2993`, `:2999`, `:3049`, `:3070`, `:3076`,
  `:3133`, `:3139`) to the registered code each case actually means, per
  version. js/net does not send joining FETCH, so it only needs the decode
  side if it reads FETCH_ERROR codes.

Additive, on main after the dev merge lands. No draft change: all three are
IETF-registered values and moq-lite already specifies the priority field.

## Closes

- [#3534](https://github.com/moq-dev/moq/issues/3534) - close this issue when the quest finishes
- [#3492](https://github.com/moq-dev/moq/issues/3492) - close this issue when the quest finishes

## Related

- [IETF error codes](https://github.com/moq-dev/moq/pull/3531) - the sweep that introduced `request::to_code` and `Kind`
- [Joining FETCH prefix](https://github.com/moq-dev/moq/pull/3562) - where the two FETCH codes were first sent
- [TRACK_STATUS answered](https://github.com/moq-dev/moq/pull/3519) - the closed PR whose adapter guard and test this reuses
- [IETF uni stream types](/quest/m2/ietf-uni-stream-types.md) - the neighbouring stream-type registry check
