# [L] Metadata-only subscriptions

## Goal

Rust and JS subscribe to a track's group and frame progress without its
payload, through the shared subscription model and whichever lite wip is
current when this lands. A chain of relays forwards and aggregates that
demand without pulling the media behind it, and a metadata subscription
becomes a payload one in place, on the same stream.

## Plan

Decided 2026-10-13 and confirmed 2026-11-02 (maintainer), carried from the
quest deleted in [moq-dev/moq#8423](https://github.com/moq-dev/moq/pull/8423)
(`git show cd560bde5^:quest/m5/route-health/meta.md`):

- ✅ A `meta: true` field on `Subscription`, carried by the existing
  SUBSCRIBE: "SUBSCRIBE_META" was shorthand, not a new message. It is a live
  stream: `largest()` on a payload-less standby is only the SUBSCRIBE_OK
  snapshot, and a one-shot status races the active route.
- ✅ Each group stream marks its mode in its own header, so a stream opened
  before a mode update is read under the mode it was opened in (rejected:
  the mode only on the subscription, which races updates against open
  streams). A metadata FRAME keeps its timestamp and declared size and
  carries no bytes, so a real empty frame stays distinguishable.
- ✅ Promotion is a SUBSCRIBE_UPDATE on the same subscription, asking for
  bytes from the reader's first missing frame, including positions it saw as
  metadata. Metadata never satisfies a media reader, fills a cache as a
  complete frame, or advances a resume cursor.
- ✅ A relay pulls payload upstream only for payload demand. Metadata demand
  travels upstream as its own live metadata subscription, served from a
  payload copy while the relay holds one. When only metadata demand remains,
  the relay demotes its upstream subscription with an update, and drops it
  when the last reader leaves.
- ✅ An older version never emulates it by fetching and discarding media: it
  reports the mode unsupported. No moq-transport counterpart is needed.

Mirror the field across Rust, JS, and every binding that exposes
`Subscription`.

Tests: metadata-only traffic across several hops carries no payload; a relay
whose last payload reader leaves keeps forwarding headers to its metadata
readers; mixed demand; promotion and demotion during an open group; empty
frames; late frames in the old mode; resume at a position seen as metadata;
cancellation; refusal on an unsupported version. Sweep tracks, readers, and
relay depth for bytes, wakes, and memory. Update
`drafts/draft-lcurley-moq-lite.md` and both codecs together, and add the mode
to `just test interop --all`.

Public API: `Subscription.meta` in Rust, JS, and the bindings. Wire: a
metadata mode in the current lite wip: the SUBSCRIBE field, a mode marker in
each group stream header, and payload-less FRAMEs.
