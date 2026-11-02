---
title: Compressed tracks
description: When to compress a track, what each protocol version receives, and what it costs a relay
---

# Compressed tracks

A publisher can mark a track compressed. Each group then travels as one DEFLATE
stream ([moq-flate](/draft/moq-flate)): every frame compresses against the
earlier frames of its group, and the next group starts a fresh window.
Applications still write and read plain frames. A reader whose session cannot
carry compression gets an inflated copy from the relay.

```rust
let info = moq_net::track::Info::default().with_compress(true);
```

```ts
const track = broadcast.createTrack("state.json", { compress: true });
```

## When to set it

Set it on small, repetitive data whose frames repeat each other within a group:
JSON state, catalogs, logs. hang's `catalog.json` and a relay's
[stats](/concept/stats) tracks already set it, and a [JSON track](/lib/rs/moq-json)
should too.

Leave it off for encoded media and anything else already compressed or
encrypted. DEFLATE cannot shrink those, and a relay would still inflate them for
every group a plain reader asks for.

## What each session receives

| Session | Receives |
| --- | --- |
| moq-lite 07 (`moq-lite-07-wip`) | The compressed bytes and the flag |
| moq-transport, when both peers offer the [Flate extension](/draft/moq-flate) | The compressed bytes and the flag |
| moq-lite 01 through 06, and moq-transport without the extension | The inflated frames, without the flag |

moq-lite 07 is still opt-in, so compressed bytes cross a hop only where both
ends list `moq-lite-07-wip`. Sessions on the default versions, and relay links
among them, carry the inflated frames. Only Rust offers the moq-transport
extension, so a JavaScript or third-party moq-transport peer reads the
inflated copy. A received track's `compress` flag says which form arrived. A
datagram compresses alone, with a fresh window.

## Cost on the relay

A relay never compresses. It stores a compressed group as received, forwards
those bytes to sessions that carry the flag, and inflates the group once, from
the first plain read. Every later plain reader of that group, on any session
and any version, shares that copy, so a thousand moq-lite 06 viewers cost one
inflate. The copy counts against the relay's [cache](/bin/relay/config#cache)
like the stored bytes and is dropped with its group.

A long compressed group sheds its oldest frames like a plain one. Once a plain
read started the inflated copy, the relay inflates each stored frame before
shedding it, so plain readers carry on past the shed front, and the copy sheds
its own oldest frames past 32 MB. A plain reader that arrives after the first
frame was shed, before any plain read started, sees `Evicted`.

Each compressed frame declares its inflated size, which meets the same group
and cache limits as a plain frame's before any inflate starts. Output that
does not match the declared size fails the group. With
[stats](/bin/relay/config#stats) enabled, `moq_relay_inflated_bytes_total` on
[`/metrics`](/bin/relay/http#get-metrics) and the `inflated`
[counter](/concept/stats) report the bytes the relay inflated.

The publisher pays for compression, holding a DEFLATE encoder of about
256 KiB while a compressed group is open.
