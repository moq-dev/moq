# Proxied streams

## Goal

A session can open `PROXY <broadcast>`: one bidirectional stream of opaque
bytes, routed hop by hop to that broadcast's publisher the way a subscription
is, with no group cache and no fan-out. Each hop holds about a stream's flow
control window. Through a hop whose version lacks PROXY, moq-net carries the
same stream as two tracks under a hidden `.proxy/` namespace and converts it
back at the next hop that has it, so an application sees one API on every
path and never branches on the version. That covers every moq-transport hop
and a moq-lite hop on a version before PROXY's, as between moq.pro's relays
mid-rollout, where two releases run side by side (AGENTS.md). A relay on
cdn.moq.pro forwards it.

Boundaries:

- The bytes are opaque. Identity, framing, and meaning are the
  application's, carried in band.
- A hop failure resets the stream, as a broken TCP path would. Only the
  `.proxy/` track form can resume, from its cache.
- No fan-out and no caching on the moq-lite form.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan from Kixel's
[stream-memory](https://github.com/kixel-ai/kixel.ai/blob/main/quest/m1/stream-memory.md)
question):

- ✅ A generic primitive here, a proxied stream to an announced broadcast,
  that Kixel's TCP byte streams use first (rejected: a TCP feature with an
  exception to AGENTS.md's rule that TCP tunnels belong to Kixel). Kixel's
  copies of a byte stream hold up to a group's 32 MiB cache per hop, and TCP
  gains nothing from a cache or fan-out.
- ✅ It reaches cdn.moq.pro too (rejected: only Kixel's own hops, which would
  need nothing here; Kixel's hops first and the CDN later).
- ✅ `PROXY <broadcast>`, reusing broadcast routing, as one bidirectional
  stream (rejected: addressing by endpoint ID, which needs its own routing).
- ✅ moq-net converts PROXY to and from moq-transport, as tracks under a
  `.proxy/` namespace (rejected: a dialer that tries PROXY and falls back to
  tracks itself; a dialer that reads the first hop's version, which cannot see
  later hops).
- ✅ The moq-transport form is Kixel's two-track byte stream, moved here
  ([moqt](/quest/m3/proxy/moqt.md)) (rejected: a fresh encoding).
- ✅ Identity is in band, the application's job (rejected: the relay vouching
  for the dialer, which needs an identity type moq-net lacks).
- ✅ Explicit `dial` and `listen` grants ([grants](/quest/m3/proxy/grants.md))
  (rejected: reading a broadcast meaning dial and publishing it meaning listen,
  which would let any viewer's token open a stream to a media publisher).
  Confirmed by the maintainer in the plan's review, 2026-11-20.

m3: it is a wire change a named consumer, Kixel, waits on, and it should not
gate the cut. kixel-ai/kixel.ai's `quest/m1/proxy.md` adopts it once
cdn.moq.wtf runs it.

Decided in the plan's review, 2026-11-20 (maintainer): ✅ the
conversion keys on each hop's negotiated version lacking PROXY, not on
moq-transport alone, so a mixed-release relay pair or a moq-lite version
before PROXY's carries the track form rather than refusing (rejected: converting on
moq-transport hops only, which breaks streams mid-rollout). This extends the
moq-transport decision above; it adds no dialer-side fallback.
