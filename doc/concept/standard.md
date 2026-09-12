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
| [moq-transport](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/) | The IETF pub/sub protocol | Drafts 14 through 21 negotiated by ALPN; [moq-lite](/concept/moq-lite) is a forward-compatible subset |
| [MSF](https://datatracker.ietf.org/doc/draft-ietf-moq-msf/) | The IETF catalog format | Read and written; broadcasts ending in `.msf` select it |
| [LOC](https://datatracker.ietf.org/doc/draft-ietf-moq-loc/) | The IETF low-overhead container | Supported as a hang container kind |
| [moq-lite](/draft/moq-lite), [hang](/draft/moq-hang), and friends | This project's own drafts | Normative for the implementation, published to the datatracker from [`drafts/`](https://github.com/moq-dev/moq/tree/main/drafts) |

## moq-transport

moq-transport is the full protocol: namespaces (broadcasts) that several
publishers may share, sub-groups for layered codecs, object-level metadata and
gaps, `FETCH` for ranges of history, joining fetches, `PUBLISH` push, and
pausing. moq-lite keeps the parts a CDN can implement without conflicts and
maps everything else to "not supported" or a harmless equivalent. The
[moq-lite page](/concept/moq-lite#what-moq-lite-leaves-out) lists the
differences.

On drafts 14–19, the Rust publisher serves relative joining `FETCH` requests
with offset zero for `NextObject` subscriptions. The fetch delivers the saved
current-group prefix, and the subscription delivers later objects. Standalone,
absolute joining, and nonzero-offset fetches are refused. Draft-20 uses
subscription fills instead. JavaScript publishing does not yet serve `FETCH`;
Rust and JavaScript subscribers request unfiltered delivery on older drafts
because they do not issue joining fetches. Other publishers may replay a cached
backlog for that filter; selecting the next group instead would leave static
tracks waiting for a group that never arrives.

Several project drafts extend the IETF wire without breaking it, since `SETUP`
ignores unknown parameters: [cluster](/draft/moq-cluster) routing hop lists,
[solicit](/draft/moq-solicit) to make announcements opt-in, and
[probe](/draft/moq-probe) for bandwidth estimation.

## MSF

The MoQ Streaming Format is a catalog, playing the role HLS playlists and SDP
do elsewhere. It overlaps with the [hang catalog](/concept/hang) and the two
will likely converge. The tools track draft-01 and hide the version on the
wire, so draft-00 catalogs still decode and init data always arrives inline.
The `stalled` rendition hint is shared between the two formats.

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
    -f mpegts -pes_payload_size 0 - \
| moq --connect https://relay.example.com --broadcast test.hang import ts

moq --connect https://relay.example.com --broadcast test.hang export ts | ffplay -
```

Add `--connect-tls-insecure` for a self-signed relay on your own test
network (it accepts any certificate, so never point it at a remote relay) and
`RUST_LOG=info,moq_net=debug` to see the negotiated version. Behavior worth
knowing when pointing another implementation at ours: we announce every
namespace we can offer unsolicited *and* ask for every prefix we may discover;
set the solicit `SETUP` option to make us wait to be asked. Single-track
`PUBLISH` offers are declined; announce a namespace and serve the resulting
`SUBSCRIBE`s instead.
