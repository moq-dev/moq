# [S] A publisher change updates a cluster advertisement in place

## Goal

Under the IETF cluster extension, an advertisement whose original publisher
(first HOP_PATH entry) changes is updated in place, like any other change,
instead of withdrawn and advertised again. A subscriber never sees a
namespace briefly vanish because its publisher moved, and the receiver still
never splices a subscription across two publishers.

## Plan

Decided 2026-09-28: match moq-lite. The lite draft already treats a route's
first hop as its identity: routes update in place, and across differing
first hops a relay must not splice a live subscription, so in-flight
subscriptions end and the subscriber re-requests. The cluster draft instead
requires PUBLISH_NAMESPACE_DONE or NAMESPACE_DONE and a fresh advertisement,
using the withdraw to force that discontinuity. The maintainer prefers
updating over quickly toggling a namespace.

- `drafts/draft-lcurley-moq-cluster.md` (Updating an Advertisement, and the
  "moves to another MUST withdraw" line under Path Selection): a publisher
  change is sent as an ordinary update (REQUEST_UPDATE, or a re-sent
  NAMESPACE). A receiver that sees the first entry change MUST NOT resume
  subscriptions on the new route and ends those served from the old
  publisher, as lite does. Add a changelog entry. Run `just drafts check`.
- Rust `rs/moq-net/src/ietf/publisher.rs` (`sync_namespace`): the
  PUBLISH_NAMESPACE path stops withdrawing on a first-hop change; the inline
  path already updates in place. The receiving relay ends subscriptions
  across a first-hop change, if it doesn't already.
- JS: [JS IETF reprice](/quest/m1/js-ietf-reprice.md) follows the new rule.
- Tests in both languages: a first-hop change sends one update and no
  withdrawal, and a subscription served by the old publisher ends rather
  than resuming on the new one.

Public API: none. Wire: the cluster extension's update semantics change (no
message or parameter changes); a peer on the old text would see an in-place
update it expected as a withdrawal, which only this tree's relays speak.

## Related

- [Cluster routing](/quest/m1/cluster-routing.md) - the routing plan these advertisements feed
