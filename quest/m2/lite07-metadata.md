# [L] Metadata mode on the lite-07 wire, served

## Goal

moq-lite-07 freezes with the wire for
[metadata-only subscriptions](/quest/m3/route-health/meta.md), so
[failover](/quest/m3/route-health/failover.md) does not wait on lite-08, and
every lite-07 publisher and relay of ours serves the mode instead of refusing
it. The draft defines a mode on SUBSCRIBE, set when it opens and never
updated, and the payload-less FRAME a metadata subscription carries. Rust and
JS (`moq-net` and `js/net`) serve a metadata SUBSCRIBE from a track they
publish or hold, and a relay forwards metadata demand it cannot serve
upstream. The public API for asking for metadata, the bindings, and failover
stay in [meta.md](/quest/m3/route-health/meta.md).

## Plan

Decided 2026-11-23 (maintainer, /quest-run): ✅ the wire change goes into
lite-07 before the cut, not the implementation (rejected: keeping it for
lite-08). Route health now has an immediate user (see the
[epic](/quest/m3/route-health/README.md)).

Decided 2026-11-25 (maintainer, /quest-run): ✅ this quest also implements
the publisher (serving) side in Rust and JS, so every lite-07 publisher and
relay serves the mode rather than refuses it; the subscriber side and
failover stay in m3's [meta.md](/quest/m3/route-health/meta.md) (rejected:
wire-only with a refusal by a lite-07 peer lacking the mode; dropping the
quest). In the maintainer's words: "we need to implement the publisher side
at least now in m2". This reverses 2026-11-23's "not the implementation" for
the serving half. Resized from M to L.

The wire, as [meta.md](/quest/m3/route-health/meta.md) recommended:

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

Serving, under meta.md's standing relay rules (✅ 2026-10-13 and 2026-11-02):

- A publisher serves a metadata SUBSCRIBE from the track it publishes with
  the same ranges, priority, drops, and skips as a payload one, writing each
  frame's header without its payload. A compressed frame reports its
  declared size without being inflated.
- A relay serves metadata from a payload copy while it holds one; otherwise
  it forwards the demand upstream as a metadata subscription, holding at most
  one upstream subscription per mode for a track. When the last payload
  reader leaves and metadata demand remains, it opens the metadata
  subscription and closes the payload one. A metadata copy never answers a
  payload reader. This needs the payload-less FRAME's decoder and a
  header-only copy inside `moq-net` and `js/net`'s forwarding, with no public
  consumer type yet; that type is meta.md's.
- Check with the TLA+ subscription model (`/verify-subscribe`) that two
  subscriptions to one track in different modes keep the existing
  accounting; with fixed modes no promotion rule is needed.

The refusal, re-examined now that every lite-07 publisher serves the mode.
Recommended: keep one stream error code, NOT_SUPPORTED at 0x30 (free in the
stream table and matching the session code), but narrow it to a relay whose
route to the track cannot carry the mode, such as an upstream on lite-06,
moq-transport, or a bridge. A lite-07 publisher MUST NOT refuse a metadata
SUBSCRIBE for a track it serves. The refusal stays because the newest and
previous relay releases interoperate during a rollout, so a lite-07 relay can
sit below a lite-06 hop, and the standing rule forbids emulating the mode by
fetching payload. A relay whose upstream refuses refuses downstream with the
same code. Recommended against: dropping the code (a mixed-version chain then
has no answer but a payload fetch or a generic error) and a SETUP capability
(a relay learns its route's support only per subscription).

Start from the draft text and codecs on `quest/m3/route-health/meta` at
`c00addff48` ([moq-dev/moq#8882](https://github.com/moq-dev/moq/pull/8882),
closed), dropping what promotion needed: `Meta` on SUBSCRIBE_UPDATE and
GROUP, and `Payload Group`/`Payload Frame`. Port it to today's SUBSCRIBE,
which has changed since. Both codecs read and write the payload-less FRAME
in this quest, since relays decode it. Pin the SUBSCRIBE byte in
`rs/moq-net/fixtures/lite.json`.

Tests, through `moq-net`'s and `js/net`'s internal entry points: a metadata
SUBSCRIBE served by a publisher carries headers and no payload bytes; an
empty frame stays distinguishable; a relay with a payload copy serves both
modes from it; a relay without one forwards metadata upstream and carries no
payload across that hop; the last payload reader leaving switches the
relay's upstream to metadata; a relay below a lite-06 hop refuses with
NOT_SUPPORTED. Cover Rust and JS serving each other in
`just test interop --all`.

Public API: none; asking for metadata is meta.md's. Wire: breaking on
`moq-lite-07-wip`: `Meta` on SUBSCRIBE, the payload-less FRAME, and the
NOT_SUPPORTED stream code.

## Related

- [Finalize moq-lite-07](/quest/m2/lite07-finalize.md) - the cut waits on this
- [Redesign lite-07 subscribe and fetch](/quest/m2/lite07-subscribe/README.md) - moves Priority and Group Order onto per-range fields, adds Previous Delta to GROUP, and removes FETCH and SUBSCRIBE_SKIP; `Meta` stays one byte on SUBSCRIBE independently
- [Metadata-only subscriptions](/quest/m3/route-health/meta.md) - the subscriber API and bindings that ask for what this serves
