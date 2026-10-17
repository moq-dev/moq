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
- **History is one group.** In Rust, Object ID gaps must be marked by Prior Object ID Gap properties for a group to be fetchable. A `FETCH` returns one group from the cache, or on drafts 14 through 19 the saved prefix of the group a new subscription just joined. A range of groups is refused, as is a draft 20+ `LOCATION_FILTER` that ends at Largest Object rather than an absolute End Location. On draft 20 and later, `FETCH_OK`'s End Location is the requested end, capped at Largest Object: a range holding no objects gets an empty fetch stream, and only a start past Largest Object is refused, with `INVALID_RANGE`. A relay's copy with no upstream subscription can't know Largest Object short of the track's end, so it echoes the requested end instead. JavaScript publishing refuses every `FETCH`. Datagrams are never fetchable. A Rust reader that only fetches learns the track from `TRACK_STATUS` instead of subscribing, so a finished track stays fetchable; draft 17 still subscribes, since its `TRACK_STATUS` answer cannot say whether the track is timed, and so does a reader whose publisher refuses `TRACK_STATUS` as `NOT_SUPPORTED`.
- **Timing.** A track whose `SUBSCRIBE_OK` declares no `TIMESCALE` is untimed, as is every track on drafts 14 through 16. A Rust reader that never subscribed takes the units from `TRACK_STATUS` on draft 18 and later.
- **Strict SETUP and parameters.** Repeated unknown `SETUP` options, GREASE included, are accepted if well-formed; a repeated known option is rejected. On draft 17 and later, a `GROUP_ORDER` other than Ascending (1) or Descending (2) closes the session. Message parameters follow the negotiated draft's lists: drafts 14 and 15 ignore an unknown or misplaced parameter, draft 16 ignores one defined only for another message but closes the session on an unknown one, and draft 17 and later close it on both. Other malformed control input closes the session with the draft's code too, such as a server `SETUP` carrying `PATH` or `AUTHORITY`, a request cut short by FIN, or a second `GOAWAY` on one stream; a first `GOAWAY` on a request stream is legal.
- **One `SETUP` credential**, forwarded to the [auth server](/bin/relay/auth#the-contract) unverified. A second `SETUP` token closes the session. We advertise no token cache, so an alias reference in `SETUP` closes it with `PROTOCOL_VIOLATION`, one on a request with `UNKNOWN_AUTH_TOKEN_ALIAS`, and a registration on a request with `AUTH_TOKEN_CACHE_OVERFLOW`. Where the Rust side authorizes the peer or verifies tokens, a token on a `SUBSCRIBE` or `PUBLISH_NAMESPACE` is verified like an in-band session token and authorizes that one request alone, even where that authorization already covers it; it never widens the session. It is refused with `NOT_SUPPORTED` when nothing verifies it, when it is empty, or when its Token Type is anything but 0 (out of band). A side that does neither, JavaScript included, ignores the token. Rust refuses a request carrying more than one distinct token with `NOT_SUPPORTED`, since it verifies one; JavaScript, which verifies none, ignores them all. A token-authorized `SUBSCRIBE` still stays within the grant the peer gave us. A token-authorized `PUBLISH_NAMESPACE` needs a grant covering everything beneath the namespace, and the tracks we subscribe under it rely on that announce instead of our limit, ending with it. An update renews the token, except on a drafts 14 and 15 `PUBLISH_NAMESPACE`, which has no update, and a refused renewal ends only that request. A token on any other request is decoded and grants nothing, and a request repeating one is refused with `MALFORMED_AUTH_TOKEN`. `SUBSCRIBE_TRACKS` reads its namespace before refusal; its token is not decoded. On draft 17 and later, the negotiated [MoQ Auth extension](/concept/moq-lite#authorization) accepts additional session tokens in band.
- **Refused, not fatal.** A legal request this stack does not serve is rejected on its own and the session stays up: `FORWARD=0`, `SUBSCRIBE_TRACKS`, and the fetch forms above get `NOT_SUPPORTED`. Range Filters get `INVALID_FILTER` on draft 19 and later because the advertised filter limit is zero. On draft 21 and later, namespaces whose first tuple field is exactly `.` or `.session` get `DOES_NOT_EXIST` before application lookup; other dot prefixes still reach the application. Subscription updates accept priority and delivery timeouts; Rust also renews request tokens. Other supported-but-unimplemented preferences end that subscription; a `TRACK_PROPERTY_FILTER` there closes the session with `PROTOCOL_VIOLATION`, since it belongs only to `SUBSCRIBE_TRACKS`. On draft 18 and later a namespace update may change its prefix; suffixes after the acknowledgement are relative to that new prefix.
- **Objects and datagrams.** Only subgroup 0 is accepted: a stream for any other subgroup, whether its header gives the ID or takes it from the first Object ID, is stopped and the session stays up. Rust relays preserve Object IDs, gaps, object properties, FIRST\_OBJECT, and END\_OF\_GROUP status objects between moq-transport peers for subgroup 0. Datagrams retain their IDs, properties, priority, and status. Plain Rust readers and moq-lite peers see consecutive frames and normal datagram payloads; JavaScript supports only normal datagrams at Object ID 0. A malformed datagram closes the session; a relayed one the downstream draft cannot carry is dropped. A relayed Timestamp leaves in the units the downstream response declares, and is dropped where it declares none (drafts 14 through 16), as for a plain frame; an immutable one that would change or vanish is refused. Only property 0x10 is read as a Timestamp. Property 0x06 stays opaque and keeps its value regardless of timestamp units; it is dropped across the draft-19 boundary, or refused when immutable, because draft 19 assigns it to SUBGROUP\_DELIVERY\_TIMEOUT. From draft 15, a status object carrying properties is refused, and one relayed from draft 14 leaves without them. Draft 14 allows them, but an END\_OF\_TRACK carrying properties ends the subscription as unsupported, since a track end has nowhere to keep them. A `FETCH` carries the END\_OF\_GROUP object on drafts 14 and 15; later drafts have no FETCH status, so their response ends at the last frame. A group cannot resume across route changes by frame ordinal once Object IDs skip ahead of the ordinals on either route; recovery refuses it instead of splicing different objects.
- **Priority.** Higher is served first. The IETF default of 128 is this stack's 127, and a track that never sets one is 127.
- **Size.** An object extension block larger than 64 KiB ends that subgroup stream. The session stays up. This cap is ours, not the draft's.

Several project drafts extend the IETF wire without breaking it, since `SETUP`
ignores unknown parameters: [cluster](/draft/moq-cluster) routing hop lists,
[solicit](/draft/moq-solicit) to make announcements opt-in,
[hidden](/draft/moq-hidden) to keep `.`-named namespaces out of discovery,
[auth](/draft/moq-auth) to tell each peer what it may publish and subscribe to,
[active-count](/draft/moq-active-count) to count the `NAMESPACE` messages
before a `SUBSCRIBE_NAMESPACE` is caught up, and
[probe](/draft/moq-probe) for bandwidth estimation.
Both libraries offer solicit and auth (draft 17 and later) unless a side's
`extensions` setting declines one, which connects it as a peer that does not
speak that extension. The pooled JavaScript `Connection` has no such setting yet.
[moq-e2ee](/draft/moq-e2ee) is not a transport extension. It encrypts
application payloads, so relays still forward named tracks they cannot read.
See [Encryption](/concept/hang#encryption).

### Deliberate deviations

These answers deliberately differ from the draft or exercise its implementation
choices. They follow the product model.

- **One publisher per path.** A broadcast path names one piece of content, so a
  `SUBSCRIBE` goes to one route, not to every publisher whose namespace matches.
  [Draft 16 §8.5](https://www.ietf.org/archive/id/draft-ietf-moq-transport-16.html#section-8.5)
  requires the relay to send that `SUBSCRIBE` to all matching publishers. See
  [publisher epochs](/concept/moq-lite#publisher-epochs) for how that one route
  is chosen.

- **Plain readers leave object properties out.**
  [Draft 18 §2.5](https://www.ietf.org/archive/id/draft-ietf-moq-transport-18.html#section-2.5)
  says a relay that does not understand a property still forwards and caches
  it. Rust moq-transport relay paths do so; the public API, plain moq-lite, and
  JavaScript [leave other per-object metadata out](/concept/moq-lite#what-moq-lite-leaves-out).
  Immutable property blocks retain their bytes. Forwarding is refused when the
  negotiated drafts use incompatible key or integer encodings, or when the block
  holds a Timestamp in units the response does not declare.

- **`SUBSCRIBE_OK` before an old source answers.** moq-lite 01 through 04 have
  no track stream, so the relay cannot learn from that source whether the track
  exists before answering. A moq-transport subscriber gets `SUBSCRIBE_OK`
  before the source answers, and a missing track ends as `PUBLISH_DONE`, not
  `REQUEST_ERROR`.
  [Draft 16 §8.4](https://www.ietf.org/archive/id/draft-ietf-moq-transport-16.html#section-8.4)
  requires an established upstream subscription before `SUBSCRIBE_OK`. From
  moq-lite 05 the track stream answers first, and a missing track is refused
  before `SUBSCRIBE_OK`.

- **An unknown token alias closes the session.** No alias is ever registered,
  so a `DELETE` or `USE_ALIAS` token on a request names one that does not exist.
  [Draft 21 §8.9](https://www.ietf.org/archive/id/draft-ietf-moq-transport-21.html#section-8.9)
  rejects the message, but `UNKNOWN_AUTH_TOKEN_ALIAS` exists only as a session
  termination code, so we close the session with it.

- **Deduplicated subscriptions and discovery.** Shared subscriptions and overlapping
  namespace prefixes are coalesced internally, so `DUPLICATE_SUBSCRIPTION` and
  `PREFIX_OVERLAP` are never sent.

- **Delivery timeouts are advisory.** `OBJECT_DELIVERY_TIMEOUT` and
  `SUBGROUP_DELIVERY_TIMEOUT` are accepted on subscriptions and their updates,
  but do not drop objects or reset subgroups. Latency enforcement uses
  presentation time instead of wall-clock delivery deadlines.

- **Rendezvous waits are capped at zero.** A `SUBSCRIBE` with a positive
  `RENDEZVOUS_TIMEOUT` gets `TIMEOUT` immediately if no publisher exists;
  absent or zero still gets `DOES_NOT_EXIST`.
  [Draft 21 §9.20.7](https://www.ietf.org/archive/id/draft-ietf-moq-transport-21.html#section-9.20.7)
  permits the publisher to cap the wait.

## MSF

The MoQ Streaming Format is a catalog, playing the role HLS playlists and SDP
do elsewhere. It overlaps with the [hang catalog](/concept/hang) and the two
will likely converge. The tools track draft-01 and hide the version on the
wire, so draft-00 catalogs still decode and init data always arrives inline.
Cross-broadcast renditions keep their MSF namespace when converted to hang;
a gateway publishing them as MSF resolves the relative reference against the
catalog broadcast's path, including the empty root path. MSF namespaces are
absolute while each side sees paths under its own connection root, so a
namespace matching the catalog's path on its trailing segments is read as the
catalog's own broadcast. That keeps draft catalogs, which repeat the publisher's
namespace on every track, playing from any root. A reference to a different
broadcast resolves only when both sides share a root.

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
