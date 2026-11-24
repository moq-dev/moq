# [M] Metadata mode on the lite-07 wire

## Goal

moq-lite-07 freezes with the wire for
[metadata-only subscriptions](/quest/m3/route-health/meta.md), so
[failover](/quest/m3/route-health/failover.md) does not wait on lite-08. The
draft defines a mode on SUBSCRIBE, set when it opens and never updated, the
payload-less FRAME a metadata subscription carries, and the refusal a lite-07
peer that does not implement the mode answers with. Rust and JS read and write
the mode and refuse it; nothing serves or asks for metadata yet.

## Plan

Decided 2026-11-23 (maintainer, /quest-run): ✅ the wire change goes into
lite-07 before the cut, not the implementation (rejected: keeping it for
lite-08). Route health now has an immediate user (see the
[epic](/quest/m3/route-health/README.md)).

The shape [meta.md](/quest/m3/route-health/meta.md) recommended, now settled
here:

- A `Meta` byte on SUBSCRIBE: 1 for metadata, 0 for payload, anything else a
  PROTOCOL_VIOLATION. SUBSCRIBE_UPDATE gains no field and GROUP no mode
  marker, since a subscription's mode is fixed for its life (decided
  2026-11-16, maintainer). DATAGRAMS gains no mode.
- On a metadata subscription's Group Streams, a FRAME keeps `Payload Length`
  as the inflated size plus one and its `Timestamp Delta`, with no
  `Inflated Size` and no payload, so a real empty frame stays distinguishable
  and GROUP_END keeps its 0.
- The draft's Metadata section states the semantics meta.md records as
  decided: a metadata frame never satisfies a payload reader, fills a cache as
  a complete frame, or advances a resume cursor; a relay serves metadata from
  a payload copy or a metadata subscription upstream, and never emulates it by
  fetching and discarding payload. A reader moving to payload opens a new
  subscription from its first missing frame with the Position or Joined
  Anchor lite-07 already has; confirm that needs no new bytes.
- The refusal resets the Subscribe Stream with a stream error code of its own.
  Recommended: NOT_SUPPORTED at 0x30, free in the stream table and matching
  the session code. A relay whose upstream refuses refuses downstream with the
  same code. This refusal is also what newest and previous moq.pro relays
  exchange while only one of them serves metadata. Recommended against: a
  SETUP capability alone. A relay can only learn per subscription whether its
  upstream serves metadata, so the refusal is needed either way, and a
  capability would just repeat it.

Start from the draft text and codecs on `quest/m3/route-health/meta` at
`c00addff48` ([moq-dev/moq#8882](https://github.com/moq-dev/moq/pull/8882),
closed), dropping what promotion needed: `Meta` on SUBSCRIBE_UPDATE and
GROUP, and `Payload Group`/`Payload Frame`. Port it to today's SUBSCRIBE,
which has changed since. The payload-less FRAME's codec lands with meta.md,
because before that no subscriber of ours asks for one. Pin the SUBSCRIBE byte
in `rs/moq-net/fixtures/lite.json`, and cover refusal in Rust, JS, and
`just test interop --all`.

Public API: none; the publisher refuses a metadata SUBSCRIBE with the new
code. Wire: breaking on `moq-lite-07-wip`: `Meta` on SUBSCRIBE, the
payload-less FRAME, and the stream code.

## Related

- [Finalize moq-lite-07](/quest/m2/lite07-finalize.md) - the cut waits on this
- [Redesign lite-07 subscribe and fetch](/quest/m2/lite07-subscribe/README.md) - moves Priority and Group Order onto per-range fields, adds Previous Delta to GROUP, and removes FETCH and SUBSCRIBE_SKIP; `Meta` stays one byte on SUBSCRIBE independently
- [Metadata-only subscriptions](/quest/m3/route-health/meta.md) - the implementation that serves this wire
