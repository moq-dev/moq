# [L] Announce compression

## Goal

A moq-lite-07 ANNOUNCE_START can reuse the path of any live announcement on
its stream, and ANNOUNCE_START and ANNOUNCE_UPDATE can reuse the tail of any
live announcement's hop chain, so repeated names and relay paths stop costing
full bytes on every announce. Decoded routes are identical to the literal
encoding. Compression is mandatory for every lite-07 endpoint to decode, and
any encoder may still send everything literally. lite-06 and older are
unchanged.

Non-goals: fewer announces (tree routing was dropped in favour of this), a
separate dictionary with its own bounds, and a production byte counter.

## Plan

### Wire

A base is a live announcement on the same stream, named by its distance back
from the next unassigned Announce ID: `d` names `next - d`, so `1` is the
latest START, and `0` means none. Only the stream's ordered history is
consulted, so there is no new state, table limit, eviction, or reset rule: the
receiver already stores every live route, and a decoded route owns its values,
so a base ending later changes nothing.

```
ANNOUNCE_START {
  Type (i) = 0x0
  Message Length (i)
  Path Base (i),
  Path Keep (i),              // leading segments of the base's suffix
  Route Prefix Suffix (s),    // remaining segments, literal
  Hops,
  Warm Route Cost (i),
  Cold Route Cost (i),
}

ANNOUNCE_UPDATE {
  Type (i) = 0x2
  Message Length (i)
  Announce ID (i),
  Hops,
  Warm Route Cost (i),
  Cold Route Cost (i),
}

Hops {
  Hop Base (i),
  Hop Count (i),
  Hop ID (i) ...,             // literal leading hops, e.g. a new origin
  Hop Keep (i),               // trailing hops copied from the base's chain
}
```

- Paths share their head, so `Path Keep` counts whole leading segments of the
  base's wire suffix (relative to the requested prefix). Segments, not bytes,
  to keep the codec simple.
- Chains share their tail: the origin differs per publisher while the relays
  behind it repeat. `Hop Keep` counts trailing entries of the base's wire hop
  list (the one excluding ANNOUNCE_OK's Hop ID), appended after the literal
  hops. The hop base is independent of the path base, since the best path
  match can arrive over a different relay path.
- PROTOCOL_VIOLATION: a base that was never assigned or is already retired, a
  non-zero keep with base 0, a keep larger than the base has, or a result that
  breaks the existing path and hop rules (`MAX_PARTS`, 32 hops, repeated
  non-zero Hop ID, duplicate live route).
- ANNOUNCE_END and ANNOUNCE_OK are unchanged.

### Implementation

- Rust (`rs/moq-net/src/lite/`): the codec is stateless today, so decoding
  moves beside `PrefixRun` (subscriber) and encoding beside `AnnounceRun`
  (publisher), which already hold the live set by Announce ID. The encoder
  picks bases from two ordered indexes over its live announcements, one keyed
  by path segments and one by reversed hop chain, so the longest shared head
  or tail is a neighbour lookup.
- JS (`js/net/src/lite/`): decode everything, encode literally (both bases 0).
- Fuzz: `fuzz.rs` round-trips single messages; add a stream-level harness that
  feeds message sequences through the stateful decoder.
- Update the MoQ-lite draft's ANNOUNCE_START and ANNOUNCE_UPDATE sections and
  its lite-07 changelog, plus `doc/concept/moq-lite.md` where it describes the
  format.

### Verification

- Codec tests for each violation, base reuse after ENDs, keep of the full
  path/chain, anonymous (0) hops, and a new stream starting empty.
- Rust encoder against JS decoder (and literal JS against Rust), plus
  `just test interop --all`.
- A benchmark of encoded announce bytes and encode/decode CPU against lite-06
  literal framing, swept over live routes and churn: a health-shaped workload
  (`<pid>/private/channel_N/stream-health-<ts>` from several origins sharing
  relay tails) and an all-unique control, whose overhead is the four zero
  base/keep bytes per START. An earlier
  prototype with separate path and chain dictionaries saved 69% on a similar
  workload; report what this shape achieves.

## Required

- moq-lite-07 ships as `moq-lite-07-wip`, off by default, so its wire can still change ([#4148](https://github.com/moq-dev/moq/pull/4148))

## Related

- [lite-07 stream count](/quest/m1/lite-stream-count.md) - the other lite-07
  wire change, landing in the same unreleased version
- [Relay memory](/quest/m1/relay-memory.md) - route state per relay, which
  this leaves unchanged
