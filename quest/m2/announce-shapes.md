# [XL] Announcement shapes in moq-lite

## Goal

A moq-lite announcement carries its shape, prefix, exact, suffix, or
prefix+suffix, and relays preserve it across hops. A subscriber can request
announcements by any shape (`**/transcode.pro`), and a publisher can
advertise one, so a fleet-wide service claims every matching path once
instead of per prefix. An exact broadcast at `/a` stays hidden from a reader
scoped to `/a/b` on every hop, not only on the origin holding it.

moq-lite only: IETF sessions stay prefix-only. Routing only: token claim
patterns keep their suffix support unchanged.

## Plan

Decided 2026-09-29 (quest-plan interview):

- [#4479](https://github.com/moq-dev/moq/pull/4479) does not land. It hid an
  exact broadcast from a narrower reader only on the origin holding it: no
  wire carries exactness, so relays install forwarded announcements through
  `Producer::dynamic` as prefix routes and the broadcast's existence leaks to
  narrower readers downstream. Exactness and suffixes are one problem, a
  route's shape on the wire, so they are one quest.
- IETF stays prefix-only: moq-transport will not support exact or suffix
  announcements (PUBLISH_NAMESPACE and NAMESPACE are prefix routes), so a
  session negotiated as IETF never carries a shape and there is no draft to
  converge with.
- The quest grew from the suffix-announce quest rather than a new m1 quest,
  because that quest already holds the benchmark-first plan and the
  version-gated wire approach both shapes need.
- Moved from m3 to m2 in the 2026-09-30 audit: it has no external gate, and
  the exact-scope leak is a real bug.
- The old gate ("a deployment needs a suffix claim a service prefix cannot
  express") is removed: the exact-scope leak is reason enough.
- Path patterns ([#4277](https://github.com/moq-dev/moq/pull/4277)) kept
  advertisements as prefixes, leaving non-prefix routing to this quest.

### Wire

The shape rides on both ANNOUNCE_REQUEST (interest) and ANNOUNCE_START
(advertisement), version- or setup-gated like `hidden`. A new announce Type is
not an option: an old receiver skips an unknown type without assigning it an
id (`rs/moq-net/src/lite/announce.rs`), so the id streams diverge. An older
peer gets prefix-only behavior; decide per shape whether downgrading it to a
prefix is safe (an exact route widened to a prefix still leaks) or whether the
route is withheld. Update `js/net` and `drafts/draft-lcurley-moq-lite.md` in
the same PR.

Today ANNOUNCE_START carries a path relative to the requested prefix, which
the receiver joins back on (`rs/moq-net/src/lite/subscriber.rs`). A suffix
interest (`**/transcode.pro`) has no literal prefix to join, and a matching
route is not its suffix, so the draft must define how each advertisement is
anchored and rebased for each interest shape, with the cross-shape overlap
cases as vectors both languages test.

Only these four shapes go on the wire. A richer interest pattern
(`pid/*/chat`) stays a consume-side filter over the widest shape that covers
it, as [#3770](https://github.com/moq-dev/moq/pull/3770) decided for every pattern
before this quest.

### Model

The origin model needs an `exact` route kind distinct from `source`: today
only a local broadcast (`source` set) is exact, and a forwarded route is
always a prefix. Carrying the kind also tightens `serves()` for a remote exact
route. An exact and a prefix route can share a path, but `sync_cursor`
presents one best entry per path and the lite announce state
(`AnnounceRun.live`, pending updates) is keyed by path alone, so today one
shape hides the other on the wire. Shape likely joins that identity. The
known exact-scope spots:

- Rust: `sync_cursor` in `rs/moq-net/src/model/origin.rs` admits an exact
  entry by prefix overlap; match it against the reader's patterns instead.
  `cursor_keeps_an_overlapping_prefix_above_its_scope` covers a prefix route
  and stays.
- JS: `Scope.projectRoutes` in `js/net/src/origin.ts` treats every entry
  above the root as covering, and `Candidate.exact` is local-only today.
- Test in both languages, locally and across a relay hop: an exact broadcast
  at `/a` and a prefix route at `/a` both survive a root-scoped hop, and read
  through a `/a/b` scope yield only the prefix. #4479's regressions and
  benchmark are a starting point.

### Benchmark first

The route table is a trie keyed by path segment (`rs/moq-net/benches/origin.rs`).
A suffix cannot walk it, so a naive match costs the whole announce table on
every announcement and every new cursor. Requests hit the same wall:
`request_broadcast` resolves through `best_route`, which walks the prefix trie
for the longest covering claim, so a suffix advertisement must also be found
by SUBSCRIBE and FETCH resolution. Longest prefix cannot rank `a/**` against
`**/z` for `a/x/z`, so define one total cross-shape precedence and tie rule
that Rust, JS, and relays share; path patterns' structural specificity is the
natural starting point.

Extend `rs/moq-net/benches/origin.rs` with suffix and exact cursors and route
lookups, each swept over publishers and subscribers, and extend
`js/net/bench/forward.ts` the same way, since `Scope.projectRoutes` scans
routes independently of Rust. The slopes decide between a reversed-segment
index and dropping suffix shapes from the quest.

A non-prefix advertisement needs authorizing:
[advertise auth](/quest/m2/processor/advertise-auth.md) scopes are prefix-only
today, and this quest extends them to the new shapes. Token patterns
(`moq-pattern`, `moq_auth::Claims`) already match suffixes and do not change.

## Related

- [Wildcard](/quest/m0/wildcard/README.md) - prefix-only advertisements and the service-prefix layout this extends
- [Cluster routing](/quest/m1/cluster-routing/README.md) - forwards announcements between relays, which must keep their shape
