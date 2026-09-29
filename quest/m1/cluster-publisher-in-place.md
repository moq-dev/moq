# [M] A publisher change updates a cluster advertisement in place

## Goal

Under the IETF cluster extension, an advertisement whose original publisher
(first HOP_PATH entry) changes is updated in place, like any other change,
instead of withdrawn and advertised again. A subscriber never sees a
namespace briefly vanish because its publisher moved, and the receiver still
never splices a subscription across two publishers.

## Plan

Decided 2026-09-28: match moq-lite. The lite draft already treats a route's
first hop as its identity: an update leaves in-flight subscriptions
undisturbed, and across differing first hops a relay must not splice a live
subscription, so when the serving session ends its subscriptions end and the
subscriber re-requests through the best remaining route. The cluster draft
instead requires PUBLISH_NAMESPACE_DONE or NAMESPACE_DONE and a fresh
advertisement, using the withdraw to force that discontinuity. The maintainer
prefers updating over quickly toggling a namespace.

- `drafts/draft-lcurley-moq-cluster.md` (Updating an Advertisement, and the
  "moves to another MUST withdraw" line under Path Selection): a publisher
  change is sent as an ordinary update (REQUEST_UPDATE, or a re-sent
  NAMESPACE) that leaves in-flight subscriptions on their old source. A
  receiver that sees the first entry change MUST NOT resume or splice them
  onto the new route; when their source ends they end, as lite does. Add a
  changelog entry. Run `just drafts check`.
- Rust `rs/moq-net/src/ietf/publisher.rs` (`sync_namespace`): the
  PUBLISH_NAMESPACE path stops withdrawing on a first-hop change; the inline
  path already updates in place.
- Receivers in both languages accept a first-hop change in place instead of
  treating it as a new advertisement: `run_publish_namespace_updates` in
  `rs/moq-net/src/ietf/subscriber.rs` and `runPublishNamespace` in
  `js/net/src/ietf/subscriber.ts` refuse it today, closing the stream, and
  Rust's inline NAMESPACE path replaces the source. In-flight subscriptions
  stay on their old source and are never spliced onto the new one.
- JS: [JS IETF reprice](/quest/m1/js-ietf-reprice.md) follows the new rule.
- Docs: `doc/bin/relay/cluster.md`, and any `doc/concept` page that
  describes the cluster extension, say a publisher change updates in place.
- Tests in both languages: a first-hop change sends one update and no
  withdrawal, the receiver applies it without closing the stream, and a
  subscription served by the old publisher keeps running until that source
  ends, then ends rather than resuming on the new one.
  Run `just test interop --all`.

Public API: none. Wire: the cluster extension's update semantics change (no
message or parameter changes).

Open question, compatibility: released moq-net (0.3.5 and later) refuses a
first-hop REQUEST_UPDATE with a retry interval of 0, which the sender reads
as never, so a moved publisher would stay withdrawn on that session during a
rolling upgrade. Options:

- Fall back: a sender whose first-hop update is refused withdraws and
  advertises fresh, ignoring that refusal's interval. No wire change; drop
  the fallback once 0.3.x relays are gone. (recommended)
- Negotiate: a new setup option gates in-place first-hop updates. Adds wire.
- Accept the break: cluster relays upgrade together.

## Related

- [Cluster routing](/quest/m1/cluster-routing.md) - the routing plan these advertisements feed
