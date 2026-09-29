# [M] A publisher change updates a cluster advertisement in place

## Goal

Under the IETF cluster extension, an advertisement whose original publisher
(first HOP_PATH entry) changes is updated in place, like any other change,
instead of withdrawn and advertised again. A subscriber never sees a
namespace briefly vanish because its publisher moved. Subscriptions already
served from the old publisher drain it until it ends; new requests take the
updated route; nothing is resumed or spliced across two publishers.

## Plan

Decided 2026-09-28: match moq-lite. A route's first hop is its identity:
routes update in place, and across differing first hops a relay must not
splice a live subscription. The cluster draft instead requires
PUBLISH_NAMESPACE_DONE or NAMESPACE_DONE and a fresh advertisement, using the
withdraw to force that discontinuity. The maintainer prefers updating over
quickly toggling a namespace.

Decided 2026-09-29 by the maintainer: a first-hop update does not end
subscriptions already served from the old publisher. In-flight tracks keep
draining the old copy until it ends on its own, and new requests take the
updated route. This replaces the earlier same-day decision to end them and
fail over to another route from the old publisher. Why: the lite draft
already leaves in-flight subscriptions "undisturbed" by ANNOUNCE_UPDATE, the
cluster draft already forbids tearing down subscriptions because an update
arrived, and Rust already drains: a front pinned to `Pin::Publisher` stops
qualifying once its route's first hop changes (`qualifies` in
`rs/moq-net/src/model/origin.rs`), and `selected(None)` in `front.rs` ends
it, which stops new requests while its live copies play out. Draining needs
no abort plumbing and no draft narrowing.

Decided 2026-09-29 by the maintainer: one rule for both kinds of front. Any
first-hop change on a front's route ends that front, anonymous or named:
in-flight tracks drain, and new requests get a fresh front. Why: pinning an
anonymous front to its route id rather than a publisher is an implementation
detail, and it should not let a new publisher inherit the old broadcast's
track info.

Decided 2026-09-29: accept the compatibility break. Released moq-net (0.3.5
and later) refuses a first-hop REQUEST_UPDATE with a retry interval of 0,
which leaves a moved publisher withdrawn on that session during a rolling
upgrade. Only this tree's relays speak the cluster extension, so there is no
withdraw fallback and no negotiation.

- `drafts/draft-lcurley-moq-cluster.md` (Updating an Advertisement, and the
  "moves to another MUST withdraw" line under Several Publishers): a
  publisher change is sent as an ordinary update (REQUEST_UPDATE, or a
  re-sent NAMESPACE). A receiver that sees the first entry change keeps
  in-flight subscriptions on the old source until it ends, serves new
  requests from the updated route, and MUST NOT resume or splice a
  subscription onto it. Add a changelog entry, and run `just drafts check`.
- Rust `rs/moq-net/src/ietf/publisher.rs` (`sync_namespace`): the
  PUBLISH_NAMESPACE path stops withdrawing on a first-hop change; the inline
  path already updates in place.
- Receivers in both languages accept a first-hop change in place instead of
  refusing it: `run_publish_namespace_updates` in
  `rs/moq-net/src/ietf/subscriber.rs` and `runPublishNamespace` in
  `js/net/src/ietf/subscriber.ts` close the stream today.
- Rust model: end an anonymous front on a first-hop change, removing an
  accidental asymmetry. A named front is pinned by publisher
  (`Pin::Publisher`), so a first-hop update disqualifies it. An anonymous
  front (first hop 0) is pinned to a route id (`Pin::Route`), which an
  in-place update keeps, so today it goes on serving new requests through the
  new publisher with the old broadcast's cached track info.
- JS lite `js/net/src/lite/subscriber.ts`: a different first hop calls
  `retract()` and announces again, so a forwarding relay withdraws the
  namespace. Emit an in-place update instead. In-flight subscriptions drain
  the old copy, and new requests neither reuse the old publisher's cached
  track info nor splice onto a live subscription, matching Rust.
- JS sender `js/net/src/ietf/publisher.ts` (`runPublishNamespaces`): once
  JS IETF reprice gives it in-place updates, a first-hop change uses them too
  instead of withdrawing.
- Docs: `doc/bin/relay/cluster.md`, and any `doc/concept` page that
  describes the cluster extension, say a publisher change updates in place,
  in-flight subscriptions drain the old publisher, and new requests take the
  new one.
- Tests in both languages: a first-hop change sends one update and no
  withdrawal; the receiver applies it without closing the stream; an
  in-flight subscription keeps receiving the old publisher's groups until
  that copy ends and never receives a group from the new one; a request
  after the update resolves through the new publisher without the old
  publisher's track info. Cover an anonymous (hop 0) to named change, and a
  lite ANNOUNCE_UPDATE in JS. Run `just test interop --all`.

Public API: none. Wire: the cluster extension's update semantics change (no
message or parameter changes), breaking first-hop updates toward released
relays as decided above.

## Required

- [JS IETF reprice](/quest/m1/js-ietf-reprice.md) - the JS IETF publisher updates a held namespace in place, which a first-hop change then reuses

## Related

- [Cluster routing](/quest/m1/cluster-routing.md) - the routing plan these advertisements feed
