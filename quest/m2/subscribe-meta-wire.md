# [L] Metadata mode on the lite-07 wire, served

## Goal

Every lite-07 relay of ours forwards metadata demand it cannot serve upstream
as a metadata subscription, so a [metadata-only subscription](/quest/m2/subscribe-meta.md)
pulls no payload across a hop no reader needs it on. `moq-net` and `js/net`
hold a header-only copy of a track beside its payload copy, read the
payload-less FRAME into it, and never answer a payload reader from it. The
public API for asking for metadata, the bindings, and what
[failover](/quest/m3/route-health/failover.md) and `moq-hls` read live in
[subscribe-meta.md](/quest/m2/subscribe-meta.md).

## Plan

Landed in [moq-dev/moq#9733](https://github.com/moq-dev/moq/pull/9733): the
wire on `moq-lite-07-wip` (a trailing `Meta` byte on SUBSCRIBE, 1 for
metadata, 0 for payload, anything else a PROTOCOL_VIOLATION; the
payload-less FRAME carrying `Payload Length` as the plain size plus one and
`Timestamp Delta`, with no `Inflated Size` and no payload), the draft's
Metadata section, the pinned bytes in `rs/moq-net/fixtures/lite.json`, and
both publishers serving a metadata SUBSCRIBE from any track they publish or
hold, a compressed frame named by its declared size without inflating it.
Until this lands, a relay of ours serves a metadata reader from its payload
copy with an ordinary payload subscription upstream: the payload costs every
hop above the last one, and over a lite-07 upstream this breaks the draft's
rule that a relay asks for what it cannot prove with a metadata subscription.
Both libraries write the payload-less FRAME but only tests read it, so the
ingest decoder here is also the first library check of its layout before the
cut.

Still standing (maintainer, 2026-11-25 and 2026-11-26, see the history of
this file and of [subscribe-meta.md](/quest/m2/subscribe-meta.md)):

- ✅ No NOT_SUPPORTED refusal: a relay whose upstream session cannot carry the
  mode (lite-06 or earlier, or moq-transport without the
  [extension](/quest/m3/route-health/ietf.md)) opens an ordinary payload
  SUBSCRIBE upstream and serves its metadata readers the headers. That route
  has one mode, so the track holds one upstream subscription on it.
  `moq-net` decides this from the negotiated version when it routes the
  subscription, never by trying the mode and falling back.
- ✅ Relay forwarding of metadata demand belongs to this quest, not
  [subscribe-meta.md](/quest/m2/subscribe-meta.md).

To build, under subscribe-meta.md's standing relay rules:

- A header-only copy keyed apart from the payload copy (a track entry per
  mode, never a reserved name), which the lite subscriber session fills from a
  metadata subscription's Group Streams. Both codecs read the payload-less
  FRAME there. A metadata copy never answers a payload reader.
- A relay serves a requested range from a payload copy only where the copy
  proves upstream would answer it the same way (the root cache rule); any
  range the copy doesn't prove, it forwards upstream and waits, as a metadata
  subscription where the upstream session is lite-07. It holds at most one
  upstream subscription per mode for a track. When the last payload reader
  leaves and metadata demand remains, it opens the metadata subscription and
  closes the payload one.
- Check with the TLA+ subscription model (`/verify-subscribe`) that two
  subscriptions to one track in different modes keep the existing
  accounting; with fixed modes no promotion rule is needed.

The closed [moq-dev/moq#8882](https://github.com/moq-dev/moq/pull/8882)
(`quest/m3/route-health/meta` at `c00addff48`) holds a header-only group
(`group::Producer::new_meta`, `write_meta`) and a header read
(`poll_read_info`) to start from, without its promotion.

Tests, through `moq-net`'s and `js/net`'s internal entry points: a relay
without a payload copy forwards metadata upstream and carries no payload
across that hop; a relay holding only group 10 of a payload copy, asked for
metadata on groups 5-10, forwards the groups its copy doesn't prove upstream
as metadata; the last payload reader leaving switches the relay's upstream to
metadata; a relay below a lite-06 hop serves metadata from a payload
subscription across that hop and carries no payload below it. Extend
`lite07_meta_interop` so Rust and JS read each other's metadata Group Streams
through their ingest.

Public API: none; asking for metadata is subscribe-meta.md's. Wire: none.

## Related

- [Redesign lite-07 subscribe and fetch](/quest/m2/lite07-subscribe/README.md) - shares the lite-07 SUBSCRIBE layout, `rs/moq-net/fixtures/lite.json`, and `rs/moq-net/verify/subscribe/`
- [Metadata-only subscriptions](/quest/m2/subscribe-meta.md) - the subscriber API and bindings that ask for what this serves
- [Metadata-only subscriptions over moq-transport](/quest/m3/route-health/ietf.md) - the extension that lets a moq-transport hop carry the mode instead of payload
