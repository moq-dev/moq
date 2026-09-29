# [L] A publisher change updates a cluster advertisement in place

## Goal

Under the IETF cluster extension, an advertisement whose original publisher
(first HOP_PATH entry) changes is updated in place, like any other change,
instead of withdrawn and advertised again. A subscriber never sees a
namespace briefly vanish because its publisher moved, and the receiver still
never splices a subscription across two publishers.

## Plan

Decided 2026-09-28: match moq-lite. A route's first hop is its identity:
routes update in place, and across differing first hops a relay must not
splice a live subscription. The cluster draft instead requires
PUBLISH_NAMESPACE_DONE or NAMESPACE_DONE and a fresh advertisement, using the
withdraw to force that discontinuity. The maintainer prefers updating over
quickly toggling a namespace.

Decided 2026-09-29: a first-hop update ends subscriptions pinned to the old
publisher, and the subscriber re-requests through the best remaining route.
The Rust model already does this for lite and IETF when the old first hop
is named: a front pinned to `Pin::Publisher` stops qualifying once its
route's first hop changes (`qualifies` in `rs/moq-net/src/model/origin.rs`),
and `selected(None)` in `front.rs` ends it. Another route from the old
publisher still qualifies, so a subscription fails over to it at a group
boundary instead of ending, as lite's same-first-hop rule already allows.

Decided 2026-09-29: accept the compatibility break. Released moq-net (0.3.5
and later) refuses a first-hop REQUEST_UPDATE with a retry interval of 0,
which leaves a moved publisher withdrawn on that session during a rolling
upgrade. Only this tree's relays speak the cluster extension, so there is no
withdraw fallback and no negotiation.

- `drafts/draft-lcurley-moq-cluster.md` (Updating an Advertisement, and the
  "moves to another MUST withdraw" line under Path Selection): a publisher
  change is sent as an ordinary update (REQUEST_UPDATE, or a re-sent
  NAMESPACE). A receiver that sees the first entry change ends the
  subscriptions served from the old publisher and MUST NOT resume or splice
  them onto the new route. Add a changelog entry.
- `drafts/draft-lcurley-moq-lite.md` (ANNOUNCE_UPDATE): narrow "in-flight
  subscriptions under the route are undisturbed" to updates that keep the
  first hop; one that changes it ends subscriptions pinned to the old
  publisher, matching the Rust model. Add a changelog entry.
- Run `just drafts check`.
- Rust `rs/moq-net/src/ietf/publisher.rs` (`sync_namespace`): the
  PUBLISH_NAMESPACE path stops withdrawing on a first-hop change; the inline
  path already updates in place.
- Receivers in both languages accept a first-hop change in place instead of
  refusing it: `run_publish_namespace_updates` in
  `rs/moq-net/src/ietf/subscriber.rs` and `runPublishNamespace` in
  `js/net/src/ietf/subscriber.ts` close the stream today. The update then ends
  subscriptions pinned to the old publisher; JS matches Rust if it doesn't
  already.
- Rust model: an anonymous front (first hop 0) is pinned to its route
  (`Pin::Route`), so it keeps serving when that route's first hop changes to
  a named publisher. End it on a first-hop change too.
- JS lite `js/net/src/lite/subscriber.ts`: a different first hop calls
  `retract()` and announces again, so a forwarding relay withdraws the
  namespace. Emit an in-place update instead, and end subscriptions pinned to
  the old publisher, matching Rust.
- JS sender `js/net/src/ietf/publisher.ts` (`runPublishNamespaces`): once
  JS IETF reprice gives it in-place updates, a first-hop change uses them too
  instead of withdrawing.
- Docs: `doc/bin/relay/cluster.md`, and any `doc/concept` page that
  describes the cluster extension, say a publisher change updates in place
  and ends subscriptions pinned to the old publisher.
- Tests in both languages: a first-hop change sends one update and no
  withdrawal, the receiver applies it without closing the stream, and a
  subscription pinned to the old publisher ends rather than resuming on the
  new one, including an anonymous (hop 0) to named change, and resumes
  instead when another route from the old publisher remains. The same for a
  lite ANNOUNCE_UPDATE in JS. Run `just test interop --all`.

Public API: none. Wire: the cluster extension's update semantics change (no
message or parameter changes), breaking first-hop updates toward released
relays as decided above.

## Required

- [JS IETF reprice](/quest/m1/js-ietf-reprice.md) - the JS IETF publisher updates a held namespace in place, which a first-hop change then reuses

## Related

- [Cluster routing](/quest/m1/cluster-routing.md) - the routing plan these advertisements feed
