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
| [moq-transport](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/) | The IETF pub/sub protocol | Drafts 18, 20, and 22 negotiated by ALPN; [moq-lite](/concept/moq-lite) is a forward-compatible subset |
| [MSF](https://datatracker.ietf.org/doc/draft-ietf-moq-msf/) | The IETF catalog format | Read and written; broadcasts ending in `.msf` select it |
| [LOC](https://datatracker.ietf.org/doc/draft-ietf-moq-loc/) | The IETF low-overhead container | Supported as a hang container kind |
| [moq-lite](/draft/moq-lite), [hang](/draft/moq-hang), [e2ee](/draft/moq-e2ee), [sign](/draft/moq-sign), and friends | This project's own drafts | Normative for the implementation, published to the datatracker from [`drafts/`](https://github.com/moq-dev/moq/tree/main/drafts) |

## moq-transport

moq-transport is the full protocol: namespaces that several publishers may
share, sub-groups, object metadata and gaps, ranged `FETCH`, push, and
pausing. moq-lite keeps the parts a CDN can implement without conflicts and
maps everything else to "not supported" or a harmless equivalent. The
[moq-lite page](/concept/moq-lite#what-moq-lite-leaves-out) lists what the
subset drops. What a peer actually observes against this implementation:

- **Pull.** Subscribers ask. Single-track `PUBLISH` offers are declined, unless they answer our `SUBSCRIBE_TRACKS` (below); announce a namespace and serve the resulting subscriptions. Announcements go out unsolicited. A client asks only for the prefixes its readers watch, and a server or relay link for every prefix it may discover. We never hold two overlapping requests at a peer that did not declare solicit: a broader one replaces the narrower ones once they are closed. The solicit `SETUP` option makes us wait to be asked. Without it, a peer also gets each match as a `NAMESPACE` on its `SUBSCRIBE_NAMESPACE` stream, so it hears the namespace twice. We never send `PUBLISH`.
- **We list no tracks by `PUBLISH`.** Rust and JavaScript refuse each `SUBSCRIBE_TRACKS` with `NOT_SUPPORTED`, and the session stays up. Our own peers list tracks over moq-lite instead.
- **A Rust TRACKS listing sends `SUBSCRIBE_TRACKS` upstream, only on demand.** Listing a broadcast from a moq-transport peer sends `SUBSCRIBE_TRACKS` with `FORWARD=0` while someone lists it, never on connect, announce, or subscribe, so a peer that refuses, resets, or ignores it affects only that listing. Each listed track's paused `PUBLISH` is its upstream copy, which a reader resumes rather than subscribing again. Only a `PUBLISH_DONE` with `TRACK_ENDED` removes a listed track, and it lists as finished; any other end fails the listing, and readers list again. Our own moq-transport publishers refuse it, so a Rust listing of one fails.
- **History is served one group at a time.** Our moq-transport publishers answer a `FETCH` only within one group, a draft-18 joining `FETCH` for the subscription's current group head, and a draft 20+ fill for the largest group's head. Whole-group fetches wait for an open group to finish. Both implementations accept and ignore `FILL_TIMEOUT`, including zero. A JavaScript subscription joins at the current group: on draft 18 it fetches the group's head with a Relative Joining `FETCH`, and from draft 20 asks for it with a fill. Datagrams are never fetchable. Subscribers still request ranges upstream, one per run of missing groups; a third-party publisher can answer those ranges and mark their gaps. A Rust reader that only fetches learns the track from `TRACK_STATUS`, so a finished track stays fetchable; a reader whose publisher refuses `TRACK_STATUS` as `NOT_SUPPORTED` still subscribes.
- **Compression.** Rust peers exchange compressed tracks when both offer the [Flate extension](/draft/moq-flate). Other peers receive one shared inflated copy. A publisher that seals the compression declaration makes that fallback unavailable: an incompatible peer is refused, preserving the sealed metadata. JavaScript does not offer the IETF extension.
- **Timing.** A track whose `SUBSCRIBE_OK` declares no `TIMESCALE` is untimed. A Rust reader that never subscribed takes the units from `TRACK_STATUS`.
- **Strict SETUP and parameters.** Repeated unknown `SETUP` options, GREASE included, are accepted if well-formed; a repeated known option is rejected. A `GROUP_ORDER` other than Ascending (1) or Descending (2) closes the session. An unknown message parameter, or one defined only for another message, closes the session. Other malformed control input closes the session with the draft's code too, such as a server `SETUP` carrying `PATH` or `AUTHORITY`, a namespace field ending in `/` (it would map to a path with an empty segment), a request cut short by FIN, or a second `GOAWAY` on one stream; a first `GOAWAY` on a request stream is legal.
- **One `SETUP` credential**, forwarded to the [auth server](/bin/relay/auth#the-contract) unverified. A second `SETUP` token closes the session. We advertise no token cache, so an alias reference in `SETUP` closes it with `PROTOCOL_VIOLATION`, one on a request with `UNKNOWN_AUTH_TOKEN_ALIAS`, and a registration on a request with `AUTH_TOKEN_CACHE_OVERFLOW`. Where the Rust side authorizes the peer or verifies tokens, a token on a `SUBSCRIBE` or `PUBLISH_NAMESPACE` is verified like an in-band session token and authorizes that one request alone, even where that authorization already covers it; it never widens the session. It is refused with `NOT_SUPPORTED` when nothing verifies it, when it is empty, or when its Token Type is anything but 0 (out of band). A side that does neither, JavaScript included, ignores the token. Rust refuses a request carrying more than one distinct token with `NOT_SUPPORTED`, since it verifies one; JavaScript, which verifies none, ignores them all. A token-authorized `SUBSCRIBE` still stays within the grant the peer gave us. A token-authorized `PUBLISH_NAMESPACE` needs a grant covering everything beneath the namespace, and the tracks we subscribe under it rely on that announce instead of our limit, ending with it. An update renews the token, and a refused renewal ends only that request. Rust and JavaScript send the token a route names (`Route::auth`) on its `PUBLISH_NAMESPACE`, and swapping it renews the request with `REQUEST_UPDATE`; the inline `NAMESPACE` that answers a solicited `SUBSCRIBE_NAMESPACE` carries no token. A token added to a reconnecting connection's auth handle rides every session it makes, so the announcement keeps it across a reconnect; one added to a single session's handle ends with that session. A token on any other request is decoded and grants nothing, and a request repeating one is refused with `MALFORMED_AUTH_TOKEN`. The negotiated [MoQ Auth extension](/concept/moq-lite#authorization) accepts additional session tokens in band.
- **Pause delivery.** `FORWARD=0` on a `SUBSCRIBE` or its update drops the subscription, closing its data streams and holding nothing upstream, while the request and its slot stay. `FORWARD=1` subscribes again as a new `SUBSCRIBE` with the same filter would, so nothing carries across the pause: its `REQUEST_OK` names the live edge it joined at. A paused update may change only priority and max age.
- **Refused, not fatal.** A legal request this stack does not serve is rejected on its own and the session stays up: `SUBSCRIBE_TRACKS`, multi-group `FETCH`, and joining requests reaching before the joined group get `NOT_SUPPORTED`. Range Filters get `INVALID_FILTER` from draft 20 because the advertised filter limit is zero. From draft 20, namespaces whose first tuple field is exactly `.` or `.session` get `DOES_NOT_EXIST` before application lookup; other dot prefixes still reach the application. Subscription updates accept priority, negotiated subscriber max age, and delivery timeouts. Rust also carries range changes, keeps bounded subscriptions updatable, renews request tokens, and judges `NEW_GROUP_REQUEST` as a join (see dynamic groups below). From draft 20, a wider or historical fill fails on its separate fetch stream while its subscription stays open; a largest-group fill hands its head to the live subgroup tail. Other supported-but-unimplemented preferences end that subscription; a `TRACK_PROPERTY_FILTER` there closes the session with `PROTOCOL_VIOLATION`, since it belongs only to `SUBSCRIBE_TRACKS`. A namespace update may change its prefix; suffixes after the acknowledgement are relative to that new prefix.
- **Objects and datagrams.** Only subgroup 0 is accepted: a stream for any other subgroup, whether its header gives the ID or takes it from the first Object ID, is stopped and the session stays up. Rust relays preserve Object IDs, gaps, object properties, FIRST\_OBJECT, and END\_OF\_GROUP status objects between moq-transport peers for subgroup 0, including across a moq-lite-07 hop between two relays that negotiate the [ietf](/draft/moq-ietf) extension. Datagrams retain their IDs, properties, priority, and status. Plain Rust readers and moq-lite peers without the extension see consecutive frames and only normal datagram payloads at Object ID 0; other datagram objects and statuses are dropped on those surfaces. JavaScript supports only normal datagrams at Object ID 0. A malformed datagram closes the session; a relayed one the downstream draft cannot carry is dropped. A relayed Timestamp leaves in the units the downstream response declares, and is dropped where it declares none, as for a plain frame; an immutable one that would change or vanish is refused. On a timed track the frame holds each object's time, so a mutable Timestamp that only repeats the immutable one is not restated; a status object has no frame, so it keeps its Timestamp among its properties. Only property 0x10 is read as a Timestamp. Property 0x06 stays opaque and keeps its value regardless of timestamp units; it is dropped between draft 18 and later drafts, or refused when immutable, because draft 20 assigns it to SUBGROUP\_DELIVERY\_TIMEOUT. A status object carrying properties is refused. A `FETCH` has no status objects, so its response ends at the last frame. A group cannot resume across route changes by frame ordinal once Object IDs skip ahead of the ordinals on either route; recovery refuses it instead of splicing different objects.
- **Dynamic groups.** A track advertises `DYNAMIC_GROUPS` when its publisher sets a refresh ratio, and a track received with `DYNAMIC_GROUPS` gets a ratio of 0. A `NEW_GROUP_REQUEST` counts as one more subscriber joining, which a `SUBSCRIBE` already is: it asks for a new group only when the newest group's bytes after its first frame exceed the ratio times the first frame. Joins coalesce until the asked-for group arrives. A Rust relay sends `NEW_GROUP_REQUEST` upstream, on the `SUBSCRIBE` or in a `REQUEST_UPDATE`, once a join wants a group past the newest it holds; a relay holding nothing yet asks for none. On a track without a ratio, an update carrying one ends the subscription with `UPDATE_FAILED`. A moq-lite-07 hop (`moq-lite-07-wip`, opt-in) carries the same demand natively: the ratio in `TRACK_INFO` and the asked-for group in `SUBSCRIBE_UPDATE`. Earlier moq-lite versions carry neither, so a track received over one never refreshes.
- **Priority.** Higher is served first. This stack's priorities run from -128 to 127 with 0 unset, and go out on the IETF wire, which ranks lower first, as `127 - priority`: an unset priority is IETF 127. An omitted subscriber or publisher priority reads as the drafts' default of IETF 128, one step below unset, so a relay forwards it unchanged. A relay's upstream subscription carries the highest priority its subscribers ask for and updates it as they come and go; its group order stays as the `SUBSCRIBE` asked, since no draft lets an update change it.
- **Size.** An object extension block larger than 64 KiB ends that subgroup stream. The session stays up. This cap is ours, not the draft's.

Several project drafts extend the IETF wire without breaking it, since `SETUP`
ignores unknown parameters: [cluster](/draft/moq-cluster) routing hop lists,
[solicit](/draft/moq-solicit) to make announcements opt-in,
[hidden](/draft/moq-hidden) to keep `.`-named namespaces out of discovery,
[auth](/draft/moq-auth) to tell each peer what it may publish and subscribe to,
[active-count](/draft/moq-active-count) to count the `NAMESPACE` messages
before a `SUBSCRIBE_NAMESPACE` is synced,
[epoch](/draft/moq-epoch) to carry [publisher epochs](/concept/moq-lite#publisher-epochs)
without changing a path, and
[probe](/draft/moq-probe) for bandwidth estimation. Rust sessions request PROBE
only while an owned `recv_bandwidth()` consumer is held. Read estimates through
its `peek()` or `changed()` methods; session statistics do not request PROBE.
Both libraries offer solicit, auth, and epoch unless a side's
`extensions` setting declines one, which connects it as a peer that does not
speak that extension. The pooled JavaScript `Connection` has no such setting yet.
[ietf](/draft/moq-ietf) goes the other way: it extends moq-lite-07 to carry this
object state between relays. moq-lite-07 is still a work in progress
(`moq-lite-07-wip`), negotiated only when both sides enable it, so a peer that
does not offer it does not carry this state. Only Rust offers the extension, when
`extensions.ietf` is set, and `moq-relay` always does on moq-lite-07.
[moq-e2ee](/draft/moq-e2ee) is not a transport extension. It encrypts
application payloads, so relays still forward named tracks they cannot read.
See [Encryption](/concept/hang#encryption). [moq-sign](/draft/moq-sign) is not
one either: it appends an Ed25519 signature to each frame, so relays still
forward and cache frames they cannot forge.

### Deliberate deviations

These answers deliberately differ from the draft or exercise its implementation
choices. They follow the product model.

- **One publisher per path.** A broadcast path names one piece of content, so a
  `SUBSCRIBE` goes to one route, not to every publisher whose namespace matches.
  [Draft 16 §8.5](https://www.ietf.org/archive/id/draft-ietf-moq-transport-16.html#section-8.5)
  requires the relay to send that `SUBSCRIBE` to all matching publishers. See
  [publisher epochs](/concept/moq-lite#publisher-epochs) for how that one route
  is chosen.

- **Plain readers leave object and track properties out.**
  [Draft 18 §2.5](https://www.ietf.org/archive/id/draft-ietf-moq-transport-18.html#section-2.5)
  says a relay that does not understand a property still forwards and caches
  it. Rust moq-transport relay paths do so, for object properties and for the
  track properties in `SUBSCRIBE_OK`, `FETCH_OK`, and `TRACK_STATUS_OK`; the public API, plain moq-lite, and
  JavaScript [leave other per-object metadata out](/concept/moq-lite#what-moq-lite-leaves-out).
  Immutable property blocks retain their bytes. Forwarding is refused when the
  negotiated drafts use incompatible key or integer encodings, or when the block
  holds a Timestamp in units the response does not declare. Property 0x06 crosses
  between draft 18 and later drafts as it does on an object. A track carrying a
  Mandatory Track Property (0x4000-0x7FFF) is refused with `UNSUPPORTED_EXTENSION`
  instead.

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
  [Draft 22 §8.9](https://www.ietf.org/archive/id/draft-ietf-moq-transport-22.html#section-8.9)
  rejects the message, but `UNKNOWN_AUTH_TOKEN_ALIAS` exists only as a session
  termination code, so we close the session with it.

- **Deduplicated subscriptions and discovery.** Shared subscriptions and overlapping
  namespace prefixes are coalesced internally, so we never send `PREFIX_OVERLAP`. Only
  a redundant draft-18 `PUBLISH` answering our own `SUBSCRIBE_TRACKS` gets
  `DUPLICATE_SUBSCRIPTION`.

- **Subscriber max age is negotiated.** The [Timestamp extension](/draft/moq-timestamp)
  carries a subscriber's `max_delay` in `SUBSCRIBE` and `REQUEST_UPDATE` only
  when the peer advertises support. Superseded groups expire against media time
  and the hop's clock; the newest group, including a quiet catalog, survives any
  budget. Relays forward their most tolerant subscriber's budget. Without the
  parameter, the publisher preserves its retained backlog. `OBJECT_DELIVERY_TIMEOUT`
  and `SUBGROUP_DELIVERY_TIMEOUT` are accepted and ignored: their per-object and
  post-FIN deadlines would also cut the newest group, so neither is a max-age budget.

- **A relay judges `NEW_GROUP_REQUEST` instead of forwarding it.**
  [Draft 22 §9.20.19](https://www.ietf.org/archive/id/draft-ietf-moq-transport-22.html#section-9.20.19)
  says a relay forwards it toward the publisher. Here every hop treats it as a
  join under the track's refresh ratio, against its own cache, and asks upstream
  only when the ratio calls for a new group. Every hop caches the same groups, so
  each reaches the same answer, and a request against a group still small enough
  to join is dropped rather than refreshed for every subscriber.

- **Rendezvous waits are capped at zero.** A `SUBSCRIBE` with a positive
  `RENDEZVOUS_TIMEOUT` gets `TIMEOUT` immediately if no publisher exists;
  absent or zero still gets `DOES_NOT_EXIST`.
  [Draft 22 §9.20.7](https://www.ietf.org/archive/id/draft-ietf-moq-transport-22.html#section-9.20.7)
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
