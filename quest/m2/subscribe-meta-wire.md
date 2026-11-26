# [L] Metadata mode on the lite-07 wire, served

## Goal

moq-lite-07-wip carries
[metadata-only subscriptions](/quest/m2/subscribe-meta.md), and every
lite-07 publisher and relay of ours serves the mode instead of refusing it.
The draft defines a mode on SUBSCRIBE, set when it opens and never updated,
and the payload-less FRAME a metadata subscription carries. Rust and JS
(`moq-net` and `js/net`) serve a metadata SUBSCRIBE from a track they publish
or hold, and a relay forwards metadata demand it cannot serve upstream, or
serves it from a payload subscription where the upstream is lite-06 or
earlier, or moq-transport. The public API for asking for metadata, the
bindings, and what [failover](/quest/m3/route-health/failover.md) and
`moq-hls` read live in [subscribe-meta.md](/quest/m2/subscribe-meta.md).

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the HLS and lite-07 audit):
✅ move both this quest and [subscribe-meta.md](/quest/m2/subscribe-meta.md)
back into `m2` on `moq-lite-07-wip` before the cut (rejected: moving only the
wire into `m2` and keeping the subscriber API in `m3`; leaving both in `m3` on
lite-08-wip). This reverses the 2026-11-25 deferral to `m3` (`lite-08-wip`):
`moq-hls` (both live playlist indexing and zero-payload archive/VOD indexing)
is an immediate consumer that otherwise pulls 100% of every own rendition's
payload across the relay mesh and object storage, and landing the subscriber
API alongside the wire avoids breaking the C `moq_subscription` ABI after
0.22.

Still standing from 2026-11-25 (maintainer, `/quest-run`,
[comment](https://github.com/moq-dev/moq/pull/9516#issuecomment-7168011073)):

- ✅ No NOT_SUPPORTED refusal: a relay whose upstream is an older version
  (lite-06 or earlier, or moq-transport) does an actual SUBSCRIBE upstream and
  drops the payload, serving the metadata subscription itself (rejected: the
  refusal narrowed to a relay whose route cannot carry the mode; no refusal
  code, answering with a generic error). This supersedes the earlier standing
  rule that an older version never emulates the mode by fetching and discarding
  payload, and 0x30 stays out of the plan.
- ✅ moq-transport gets an extension for metadata-only subscriptions, in
  [its own quest](/quest/m3/route-health/ietf.md); until it lands a
  moq-transport upstream is served like lite-06.
- ✅ Relay forwarding of metadata demand belongs to this quest, not
  [subscribe-meta.md](/quest/m2/subscribe-meta.md), so this quest stays L and
  subscribe-meta.md stays M.

The wire, as [subscribe-meta.md](/quest/m2/subscribe-meta.md) recommended:

- A `Meta` byte on SUBSCRIBE: 1 for metadata, 0 for payload, anything else a
  PROTOCOL_VIOLATION. SUBSCRIBE_UPDATE gains no field and GROUP no mode
  marker, since a subscription's mode is fixed for its life (decided
  2026-11-16, maintainer). DATAGRAMS gains no mode.
- On a metadata subscription's Group Streams, a FRAME keeps `Payload Length`
  as the inflated size plus one and its `Timestamp Delta`, with no
  `Inflated Size` and no payload, so a real empty frame stays distinguishable
  and GROUP_END keeps its 0.
- The draft's Metadata section states the semantics subscribe-meta.md records
  as decided: a metadata frame never satisfies a payload reader, fills a cache
  as a complete frame, or advances a resume cursor; a relay serves metadata
  from a payload copy, a metadata subscription upstream, or, where the upstream
  cannot carry the mode, a payload subscription whose payload it drops. No
  stream error code is added. A reader moving to payload opens a new
  subscription from its first missing frame with the Position or Joined
  Anchor lite-07 already has; confirm that needs no new bytes.

Serving, under subscribe-meta.md's standing relay rules (✅ 2026-10-13 and
2026-11-02, amended 2026-11-25 above):

- A publisher serves a metadata SUBSCRIBE from the track it publishes with
  the same ranges, priority, and drops (`OLD` and `SKIP`) as a payload one,
  writing each frame's header without its payload. A compressed frame reports
  its declared size without being inflated.
- A relay serves a requested range from a payload copy only where the copy
  proves upstream would answer it the same way (the root cache rule); any
  range the copy doesn't prove, it forwards upstream and waits: as a
  metadata subscription where the upstream session is lite-07, or as the
  payload SUBSCRIBE the next rule describes where it isn't. It holds at most
  one upstream subscription per mode for a track. When the last payload
  reader leaves and metadata demand remains, it opens the metadata
  subscription and closes the payload one. A metadata copy never answers a
  payload reader. This needs the payload-less FRAME's decoder and a
  header-only copy inside `moq-net` and `js/net`'s forwarding, with no public
  consumer type yet; that type is subscribe-meta.md's.
- A relay whose upstream session cannot carry the mode (lite-06 or earlier,
  or moq-transport without the [extension](/quest/m3/route-health/ietf.md))
  opens an ordinary payload SUBSCRIBE upstream and serves its metadata
  readers the headers, dropping the payload for them. That route has one
  mode, so the track holds one upstream subscription on it, and the payload
  costs only that hop. `moq-net` decides this from the negotiated version
  when it routes the subscription, never by trying the mode and falling
  back.
- Check with the TLA+ subscription model (`/verify-subscribe`) that two
  subscriptions to one track in different modes keep the existing
  accounting; with fixed modes no promotion rule is needed.

Start from the draft text and codecs on `quest/m3/route-health/meta` at
`c00addff48` ([moq-dev/moq#8882](https://github.com/moq-dev/moq/pull/8882),
closed), dropping what promotion needed: `Meta` on SUBSCRIBE_UPDATE and
GROUP, and `Payload Group`/`Payload Frame`. Port it to lite-07's current
SUBSCRIBE layout. Both codecs read and write the payload-less FRAME in this
quest, since relays decode it. Pin the SUBSCRIBE byte in
`rs/moq-net/fixtures/lite.json`.

Tests, through `moq-net`'s and `js/net`'s internal entry points: a metadata
SUBSCRIBE served by a publisher carries headers and no payload bytes; an
empty frame stays distinguishable; a relay with a payload copy serves both
modes from it; a relay holding only group 10 of a payload copy, asked for
metadata on groups 5-10, forwards the groups its copy doesn't prove upstream;
a relay without one forwards metadata upstream and carries no payload across
that hop; the last payload reader leaving switches the relay's upstream to
metadata; a relay below a lite-06 hop serves metadata from a payload
subscription across that hop and carries no payload below it. Cover Rust and
JS serving each other in `just test interop --all`.

Public API: none; asking for metadata is subscribe-meta.md's. Wire: on
`moq-lite-07-wip`, `Meta` on SUBSCRIBE and the payload-less FRAME; lite-06
and earlier unchanged.

## Related

- [Metadata-only subscriptions](/quest/m2/subscribe-meta.md) - the subscriber API and bindings that ask for what this serves
- [Metadata-only subscriptions over moq-transport](/quest/m3/route-health/ietf.md) - the extension that lets a moq-transport hop carry the mode instead of payload
