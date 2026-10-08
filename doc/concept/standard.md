---
title: Standards
description: How this project relates to the IETF moq-transport, MSF, and LOC drafts
---

# Standards

The [IETF MoQ working group](https://datatracker.ietf.org/group/moq/about/)
standardizes Media over QUIC. This project tracks that work and interoperates
with it, while shipping a simpler profile you can use today.

| Spec | Scope | Here |
| --- | --- | --- |
| [moq-transport](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/) | The IETF pub/sub protocol | Drafts 14 through 22 negotiated by ALPN; [moq-lite](/concept/moq-lite) is a forward-compatible subset |
| [MSF](https://datatracker.ietf.org/doc/draft-ietf-moq-msf/) | The IETF catalog format | Read and written; broadcasts ending in `.msf` select it |
| [LOC](https://datatracker.ietf.org/doc/draft-ietf-moq-loc/) | The IETF low-overhead container | Supported as a hang container kind |
| [moq-lite](/draft/moq-lite), [hang](/draft/moq-hang), [e2ee](/draft/moq-e2ee), and friends | This project's own drafts | Normative for the implementation, published to the datatracker from [`drafts/`](https://github.com/moq-dev/moq/tree/main/drafts) |

## moq-transport

moq-transport is the full protocol: namespaces that several publishers may
share, sub-groups, object metadata and gaps, ranged `FETCH`, push, and
pausing. moq-lite keeps the parts a CDN can implement without conflicts and
maps everything else to "not supported" or a harmless equivalent. The
[moq-lite page](/concept/moq-lite#what-moq-lite-leaves-out) lists what the
subset drops. What a peer actually observes against this implementation:

- **Pull.** Subscribers ask. Single-track `PUBLISH` offers are declined; announce a namespace and serve the resulting subscriptions. Announcements go out unsolicited, and we also ask for every prefix we may discover. The solicit `SETUP` option makes us wait to be asked. Without it, a draft 16+ peer also gets each match as a `NAMESPACE` on its `SUBSCRIBE_NAMESPACE` stream, so it hears the namespace twice. We never send `PUBLISH`: on drafts 16 and 17 a `SUBSCRIBE_NAMESPACE` asking only for `PUBLISH` is refused, one asking for both gets only `NAMESPACE`, and any other Subscribe Options value closes the session.
- **History is one group.** A `FETCH` returns one group from the cache, or on drafts 14 through 19 the saved prefix of the group a new subscription just joined. A range of groups is refused, as is a draft 20+ `LOCATION_FILTER` that ends at Largest Object rather than an absolute End Location. On draft 20 and later, `FETCH_OK`'s End Location is the requested end, capped at the track's last object: a range holding no objects gets an empty fetch stream, and only a start past the track's last object is refused, with `INVALID_RANGE`. A relay without a live feed can't know Largest Object, so it is not used as the cap. JavaScript publishing refuses every `FETCH`. Datagrams are never fetchable. A Rust reader that only fetches learns the track from `TRACK_STATUS` instead of subscribing, so a finished track stays fetchable; draft 17 still subscribes, since its `TRACK_STATUS` answer cannot say whether the track is timed.
- **Timing.** A track whose `SUBSCRIBE_OK` declares no `TIMESCALE` is untimed, as is every track on drafts 14 through 16. A Rust reader that never subscribed takes the units from `TRACK_STATUS` on draft 18 and later.
- **Strict SETUP and parameters.** Repeated unknown `SETUP` options, GREASE included, are accepted if well-formed; a repeated known option is rejected. On draft 17 and later, a `GROUP_ORDER` other than Ascending (1) or Descending (2) closes the session.
- **One credential per session**, carried in `SETUP` and forwarded to the [auth server](/bin/relay/auth#the-contract) unverified. A token attached to an individual request is ignored. An alias reference, a second token, or a parameter the negotiated draft does not define closes the session.
- **Refused, not fatal.** A legal request this stack does not serve is rejected on its own and the session stays up: `FORWARD=0`, range filters, `TRACK_STATUS` to a JavaScript publisher, `SUBSCRIBE_TRACKS`, and the fetch forms above. On draft 19 and later, and in Rust on drafts 14 through 16, a subscription update may change only priority; any other update ends that subscription.
- **Datagrams** are a single normal object at object 0, forwarded without renumbering. Anything else is dropped. JavaScript does not carry datagrams on moq-transport.
- **Priority.** Higher is served first. The IETF default of 128 is this stack's 127, and a track that never sets one is 127.
- **Size.** An object extension block larger than 64 KiB ends that subgroup stream. The session stays up. This cap is ours, not the draft's.

Several project drafts extend the IETF wire without breaking it, since `SETUP`
ignores unknown parameters: [cluster](/draft/moq-cluster) routing hop lists,
[solicit](/draft/moq-solicit) to make announcements opt-in,
[hidden](/draft/moq-hidden) to keep `.`-named namespaces out of discovery,
[active-count](/draft/moq-active-count) to count the `NAMESPACE` messages
before a `SUBSCRIBE_NAMESPACE` is caught up, and
[probe](/draft/moq-probe) for bandwidth estimation.
[moq-e2ee](/draft/moq-e2ee) is not a transport extension. It encrypts
application payloads, so relays still forward named tracks they cannot read.
See [Encryption](/concept/hang#encryption).

### Deliberate deviations

These three answers differ from the draft on purpose. They are the
product's model, not bugs, and the relay does not change them.

- **One publisher per path.** A broadcast path names one piece of content, so a
  `SUBSCRIBE` goes to one route, not to every publisher whose namespace matches.
  [Draft 16 §8.5](https://www.ietf.org/archive/id/draft-ietf-moq-transport-16.html#section-8.5)
  requires the relay to send that `SUBSCRIBE` to all matching publishers. See
  [publisher epochs](/concept/moq-lite#publisher-epochs) for how that one route
  is chosen.
- **Unknown object properties are dropped.**
  [Draft 18 §2.5](https://www.ietf.org/archive/id/draft-ietf-moq-transport-18.html#section-2.5)
  says a relay that does not understand a property still forwards and caches
  it. The model keeps a payload and a timestamp, and
  [leaves other per-object metadata out](/concept/moq-lite#what-moq-lite-leaves-out),
  so a property it does not understand stops at the session that delivered it.
- **`SUBSCRIBE_OK` before an old source answers.** moq-lite 01 through 04 have
  no track stream, so the relay cannot learn from that source whether the track
  exists before answering. A moq-transport subscriber gets `SUBSCRIBE_OK`
  before the source answers, and a missing track ends as `PUBLISH_DONE`, not
  `REQUEST_ERROR`.
  [Draft 16 §8.4](https://www.ietf.org/archive/id/draft-ietf-moq-transport-16.html#section-8.4)
  requires an established upstream subscription before `SUBSCRIBE_OK`. From
  moq-lite 05 the track stream answers first, and a missing track is refused
  before `SUBSCRIBE_OK`.

## MSF

The MoQ Streaming Format is a catalog, playing the role HLS playlists and SDP
do elsewhere. It overlaps with the [hang catalog](/concept/hang) and the two
will likely converge. The tools track draft-01 and hide the version on the
wire, so draft-00 catalogs still decode and init data always arrives inline.

## LOC

The Low Overhead Container carries a timestamp and a few properties per frame
with none of CMAF's per-frame `moof` cost. It is close to hang's `legacy`
container and is selectable per track (`container=loc` in the
[GStreamer plugin](/bin/gstreamer)).

## Interop testing

`moq-cli` speaks every listed draft, picks the newest one the relay also
supports, and prints it in the logs. Publish a test pattern and play it back:

```bash
ffmpeg -re -f lavfi -i testsrc=size=1280x720:rate=30 -f lavfi -i sine=frequency=440 \
    -c:v libx264 -preset ultrafast -tune zerolatency -g 60 -c:a aac \
    -f mpegts -pes_payload_size 0 -muxdelay 0 - \
| moq --connect https://relay.example.com --broadcast test.hang import ts

moq --connect https://relay.example.com --broadcast test.hang export ts | ffplay -
```

Add `--connect-tls-insecure` for a self-signed relay on your own test
network (it accepts any certificate, so never point it at a remote relay) and
`RUST_LOG=info,moq_net=debug` to see the negotiated version. The limits above
are the ones that surprise another implementation.
