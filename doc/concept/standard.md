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

moq-transport is the full protocol: namespaces (broadcasts) that several
publishers may share, sub-groups for layered codecs, object-level metadata and
gaps, `FETCH` for ranges of history, joining fetches, `PUBLISH` push, and
pausing. moq-lite keeps the parts a CDN can implement without conflicts and
maps everything else to "not supported" or a harmless equivalent. The
[moq-lite page](/concept/moq-lite#what-moq-lite-leaves-out) lists the
differences.

On draft 19 and later, a requester can FIN its request stream while its
subscription or namespace advertisement stays active. Cancellation uses
`RESET_STREAM` or `STOP_SENDING`. Subscription `REQUEST_UPDATE` can change
subscriber priority; other changes are refused with `NOT_SUPPORTED` and end
the subscription with `UPDATE_FAILED`. Drafts 17 and 18 retain FIN cancellation.
In Rust, drafts 14 to 16 apply a subscription's `SUBSCRIBE_UPDATE`
(`REQUEST_UPDATE` on draft 16) the same way. Draft 14 sends no answer and has
no `UPDATE_FAILED`, so a refused update ends the subscription with
`INTERNAL_ERROR`. On those drafts an update to any other request is ignored.

Rust and JavaScript accept repeated unknown `SETUP` options, including GREASE,
while still requiring every value to be well-formed. Repeated known options
are rejected. On draft-17 and later, a `GROUP_ORDER` message parameter must be
Ascending (1) or Descending (2); any other value closes the session with
`PROTOCOL_VIOLATION`.

Rust and JavaScript subscribers accept object extension blocks up to 64 KiB.
This is an implementation limit, not a limit in the IETF draft. A larger
declared block stops its subgroup stream with `MALFORMED_TRACK` before reading
the block; other groups and the session stay open.

Rust and JavaScript read an incoming padding stream (draft-18 and later) to
the end and discard it, without sending `STOP_SENDING`. A unidirectional stream type the negotiated draft does not
define, or a `SUBGROUP_HEADER` type it marks invalid, closes the session with
`PROTOCOL_VIOLATION`, as the draft requires.

An IETF publisher declares the track's default priority in `SUBSCRIBE_OK` or
`PUBLISH` when that draft carries track properties. Groups without a priority
flag inherit it. If the property is absent, the IETF wire default of 128 maps
to model priority 127, where higher values are served first. A track that
never sets a priority is 127 as well, so it goes out as 128 on IETF and 127
on moq-lite.

On drafts 14–19, the Rust publisher answers a standalone `FETCH` within one
group from the cache. A relay fetches a missing group upstream with a `FETCH`
of that one whole group, and an upstream refusal is the refusal the fetcher
sees. Once its last reader leaves, the relay cancels the upstream fetch, even
before `FETCH_OK`, and aborts an incomplete group instead of caching it as whole.
Drafts 14–16 use `FETCH_CANCEL`; drafts 17–19 stop and reset the request stream.
A range touching several groups is refused with `NOT_SUPPORTED`, as is
any `FETCH` on draft-20 and later, which moved the range into
`LOCATION_FILTER`. A standalone `FETCH` keeps each object's Timestamp, in the
units the track's `SUBSCRIBE_OK` declares. `FETCH_OK` doesn't declare them yet,
so a reader that never subscribed reads the objects untimed.

A track whose `SUBSCRIBE_OK` declares no `TIMESCALE` is untimed: its objects
arrive untimed, and an object-scope Timescale or a Timestamp on them is
ignored, as on imquic's LOC objects. A relay serves such a track downstream
untimed, without a `TIMESCALE` of its own. On a track that declares one, an
object without a Timestamp is malformed. Drafts 14–16 can't carry `TIMESCALE`,
so every track there is untimed.

On drafts 14–19, the Rust publisher also serves relative and absolute joining
`FETCH` requests for `NextObject` subscriptions, for the subscription group's
saved prefix only, while the subscription delivers later objects. One reaching
back to earlier groups is refused with `NOT_SUPPORTED`. Draft-20 uses
subscription fills instead. JavaScript
publishing refuses every `FETCH` with `NOT_SUPPORTED`;
Rust and JavaScript subscribers request unfiltered delivery on older drafts
because they do not issue joining fetches. Other publishers may replay a cached
backlog for that filter; selecting the next group instead would leave static
tracks waiting for a group that never arrives.

A moq-lite datagram is a single-frame group, so on moq-transport it travels
as an `OBJECT_DATAGRAM` at object 0 whose Group ID is the sequence, and a relay
forwards it without renumbering. A datagram carrying any other Object ID, or a
status other than Normal, is dropped, as is one for an alias not yet bound. A
malformed one closes the session. Datagrams are never fetchable: a fetch
object flagged as a datagram fails only that fetch, as `NotFetchable`, and a
relay answers its own downstream FETCH with `DOES_NOT_EXIST`, or `NOT_FETCHABLE`
to a moq-lite-07 peer. Rust and JavaScript both carry datagrams on every
supported draft.

A client may present one credential in its `SETUP` with the `AUTHORIZATION
TOKEN` option. The server reads a value (`USE_VALUE`, or `REGISTER`, which it
treats as a value since it advertises no token cache) and hands its Token Type
and bytes to the application unverified; a relay forwards them to its
[auth server](/bin/relay/auth#the-contract). An alias reference (`DELETE`,
`USE_ALIAS`) closes the session with `PROTOCOL_VIOLATION`, a structure that
does not decode with `KEY_VALUE_FORMATTING_ERROR`, and a second token is
refused. An `AUTHORIZATION TOKEN` parameter on a request is read and ignored:
the session's credential is what authorizes it.

A legal request that is not served is refused on its own with `NOT_SUPPORTED`,
leaving the session open: a `SUBSCRIBE` with `FORWARD=0`, a `SUBSCRIBE` or
`FETCH` carrying Range Filters (no `MAX_FILTER_RANGES` is advertised), a
`FETCH` carrying `FILL_TIMEOUT` (Timed-Out gaps are not written),
`TRACK_STATUS`, `SUBSCRIBE_TRACKS` (draft-18 and later), and the `FETCH`
forms above. `NEW_GROUP_REQUEST` is ignored, as
the draft allows a publisher without dynamic groups to do. A parameter the
negotiated draft does not define still closes the session with
`PROTOCOL_VIOLATION`, as the draft requires.

Several project drafts extend the IETF wire without breaking it, since `SETUP`
ignores unknown parameters: [cluster](/draft/moq-cluster) routing hop lists,
[solicit](/draft/moq-solicit) to make announcements opt-in,
[hidden](/draft/moq-hidden) to keep `.`-named namespaces out of discovery,
[active-count](/draft/moq-active-count) to count the `NAMESPACE` messages
before a `SUBSCRIBE_NAMESPACE` is caught up, and
[probe](/draft/moq-probe) for bandwidth estimation.
[moq-e2ee](/draft/moq-e2ee) is not a transport extension: it encrypts application
payloads so relays still forward named tracks they cannot read.

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
`RUST_LOG=info,moq_net=debug` to see the negotiated version. Behavior worth
knowing when pointing another implementation at ours: we announce every
namespace we can offer unsolicited *and* ask for every prefix we may discover;
set the solicit `SETUP` option to make us wait to be asked. A peer that does not
set that option still gets each match as a `NAMESPACE` on its `SUBSCRIBE_NAMESPACE`
stream on draft-16 and later, so it hears the namespace twice. We never send
`PUBLISH`: on drafts 16 and 17 a `SUBSCRIBE_NAMESPACE` asking only for `PUBLISH`
is refused, one asking for both gets only `NAMESPACE`, and any other Subscribe
Options value closes the session. Single-track
`PUBLISH` offers are declined; announce a namespace and serve the resulting
`SUBSCRIBE`s instead.
