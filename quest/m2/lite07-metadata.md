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
upstream, or serves it from a payload subscription where the upstream is an
older version. The public API for asking for metadata, the bindings, and failover
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
  a payload copy, a metadata subscription upstream, or, where the upstream
  cannot carry the mode, a payload subscription whose payload it drops. No
  stream error code is added. A reader moving to payload opens a new
  subscription from its first missing frame with the Position or Joined
  Anchor lite-07 already has; confirm that needs no new bytes.

Serving, under meta.md's standing relay rules (✅ 2026-10-13 and 2026-11-02,
amended 2026-11-25 below):

- A publisher serves a metadata SUBSCRIBE from the track it publishes with
  the same ranges, priority, drops, and skips as a payload one, writing each
  frame's header without its payload. A compressed frame reports its
  declared size without being inflated.
- A relay serves a requested range from a payload copy only where the copy
  proves upstream would answer it the same way (the root cache rule); any
  range the copy doesn't prove, it forwards upstream and waits: as a
  metadata subscription where the upstream session carries the mode, or as
  the payload SUBSCRIBE the next rule describes where it doesn't. It holds at
  most one upstream subscription per mode for a track. When the last payload
  reader leaves and metadata demand remains, it opens the metadata
  subscription and closes the payload one. A metadata copy never answers a
  payload reader. This needs the payload-less FRAME's decoder and a
  header-only copy inside `moq-net` and `js/net`'s forwarding, with no public
  consumer type yet; that type is meta.md's.
- A relay whose upstream session cannot carry the mode (lite-06 or earlier,
  or moq-transport) opens an ordinary payload SUBSCRIBE upstream and serves
  its metadata readers the headers, dropping the payload for them. That
  route has one mode, so the track holds one upstream subscription on it, and
  the payload costs only that hop. `moq-net` decides this from the
  negotiated version when it routes the subscription, never by trying the
  mode and falling back.
- Check with the TLA+ subscription model (`/verify-subscribe`) that two
  subscriptions to one track in different modes keep the existing
  accounting; with fixed modes no promotion rule is needed.

Decided 2026-11-25 (maintainer, /quest-run,
[comment](https://github.com/moq-dev/moq/pull/9516#issuecomment-7168011073)):

- ✅ No NOT_SUPPORTED refusal: a relay whose upstream is an older version
  does an actual SUBSCRIBE upstream and drops the payload, serving the
  metadata subscription itself, and moq-transport gets an extension for
  metadata-only subscriptions (rejected: the refusal narrowed to a relay
  whose route cannot carry the mode; no refusal code, answering with a
  generic error). This supersedes meta.md's standing ✅ rule that an older
  version never emulates the mode by fetching and discarding payload; the
  newer decision wins, and 0x30 leaves the plan. The extension is
  [its own quest](/quest/m3/route-health/ietf.md), so until it lands a
  moq-transport upstream is served like lite-06.
- ✅ Relay forwarding of metadata requests lands here in m2, so this quest
  stays L and meta.md M (rejected: forwarding stays in m3).

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
modes from it; a relay holding only group 10 of a payload copy, asked for
metadata on groups 5-10, forwards the groups its copy doesn't prove upstream; a relay without one forwards metadata upstream and carries no
payload across that hop; the last payload reader leaving switches the
relay's upstream to metadata; a relay below a lite-06 hop serves metadata
from a payload subscription across that hop and carries no payload below it.
Cover Rust and JS serving each other in
`just test interop --all`.

Public API: none; asking for metadata is meta.md's. Wire: breaking on
`moq-lite-07-wip`: `Meta` on SUBSCRIBE and the payload-less FRAME.

## Related

- [Finalize moq-lite-07](/quest/m2/lite07-finalize.md) - the cut waits on this
- [Redesign lite-07 subscribe and fetch](/quest/m2/lite07-subscribe/README.md) - moves Priority and Group Order onto per-range fields, adds Previous Delta to GROUP, and removes FETCH and SUBSCRIBE_SKIP; `Meta` stays one byte on SUBSCRIBE independently
- [Metadata-only subscriptions](/quest/m3/route-health/meta.md) - the subscriber API and bindings that ask for what this serves
- [Metadata-only subscriptions over moq-transport](/quest/m3/route-health/ietf.md) - the extension that lets a moq-transport hop carry the mode instead of payload
