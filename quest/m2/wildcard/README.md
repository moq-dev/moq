# Wildcard advertisements

## Goal

A service can advertise a path pattern it could
serve rather than enumerating every broadcast matching it. A wildcard is priced
at what starting the work would cost, so a running publisher wins on the metric
routing already minimizes, and retracting the wildcard stops new work without
shedding what is already running.

Three workloads need this, and they are the three pattern shapes. A transcode
worker today announces a standby derivative for every matching live broadcast,
so announcements scale as workers times broadcasts; with a suffix pattern
`**/transcode.pro` it advertises once for the whole fleet. A chat backend
cannot enumerate at all: rooms exist independently of any broadcast, and the
subtree pattern `<pid>/chat/**` expresses them. An archive serving recordings
over FETCH wants to say "if nobody is publishing this live, I have it", which
is the catch-all `**`, a claim about every path at once.

The cost of enumerating is measured, not assumed.
[relay-memory](/quest/m2/relay-memory/README.md) puts one announcement at
8.8 KB per relay with one route, plus 4.3 KB per additional route, and every
relay that hears it materializes that whether or not anything there subscribes.
So "workers times broadcasts" is not just a large number of messages; it is that
number multiplied across the fleet in resident memory, which is the same growth
that questline exists to stop.

## Plan

### What already exists, and what does not

Route cost already names this case: "The original publisher seeds it with its
production cost: zero for a live publish, something large for a standby that
would have to start working (a cold transcoder)"
(`drafts/draft-lcurley-moq-lite.md`). `moq_token::Claims.publish` and
`origin::Producer` gain versioned patterns through
[Path patterns](/quest/m2/path-patterns/README.md), so advertisements reuse the
same exact containment check. [#2925](https://github.com/moq-dev/moq/pull/2925)
has since replaced `RouteCost` with `Cost { warm, cold }`.

Matching, by contrast, is prefix-only everywhere today. The pattern matcher is
not this questline's to build: path authorization adopts the same dialect first,
and its [Matcher](/quest/m2/path-patterns/matcher.md) quest delivers the shared
matching, containment, and rebasing that [Advertise](/quest/m2/wildcard/advertise.md) now requires.

Two things look built and are not:

1. **A routing table.** `origin::Dynamic` (`rs/moq-net/src/model/origin.rs`)
   serves an unannounced path on demand and `lite::publisher::recv_subscribe`
   already falls back to it. It is not usable here. Its queue is keyed by path
   alone, so requests from peers with different `exclude` origins coalesce onto
   one entry; a `Request` carries no requester identity, hop list, or cost; and
   every live handler drains one shared FIFO, so N wildcards cannot be ranked.
   `request_broadcast` says so itself: "a handler resolves paths with no route
   chain to check, so falling through would let it route around the split
   horizon and rebuild the loop."
2. **Announcement `Epoch`.** [#2611](https://github.com/moq-dev/moq/pull/2611)
   specced it into lite-06 and [#1920](https://github.com/moq-dev/moq/pull/1920)
   had earlier removed the one that used to exist in code, so it is specified and
   unimplemented. It is absent from `dev` too, whose `AnnounceBroadcast::Active`
   carries `{ suffix, hops, cost }` and no epoch. `broadcast.rs`'s `route_epoch`
   is an unrelated route-change counter. [Epoch](/quest/m2/wildcard/epoch.md) owns closing that gap.

### Decisions

- **One pattern: the [path-patterns](/quest/m2/path-patterns/README.md)
  dialect.** An advertisement carries the same path pattern token rules use
  (literal, `*`, or `lit*lit` segments, at most one `**`), matched by the same
  shared matcher, so nothing resembles a second grammar. Exact set-valued
  rebasing preserves every match inside a rooted view, including both the root
  and deeper residuals when `**` consumes zero or more segments.
- **Most specific pattern wins, and its refusal is final.** When several
  patterns match one path, only the tier selected by the matcher's shared
  structural specificity is consulted; equal-specificity patterns
  form one pool that cost and the request hash order. A terminal refusal from
  the winning tier IS the answer and never falls through to a less specific
  pattern, so a transcoder refusing a path does not leak the request to the
  archive's catch-all, and one unserved path still costs one round trip. The
  capacity re-resolution below stays within the tier, refuser excluded. The
  accepted consequence: an offline derivative (a recording of
  `foo.hang/transcode.pro`) is not reachable through the catch-all, because
  the more specific transcode pattern shadows it.
- **A wildcard is a POOL, not a competitor.** Several advertisers of one
  pattern is the normal state, not a hazard: every transcode worker advertises
  `**/transcode.pro` and takes a share. What distributes them is a
  deterministic hash of the REQUESTED path against each advertiser, so distinct
  paths spread rather than one advertiser winning the whole pattern.
  Distribution is the requirement, not any particular pair: a correct hash may
  legitimately rank the same advertiser first for two given paths, so what must
  hold is that a large path set spreads and that one path always resolves the
  same way. Cost orders the pool first, which keeps work local and makes a
  distant advertiser the overflow rather than an equal peer.
- **A wildcard is priced, not special-cased.** Within a tier, route selection
  stays one comparison on one metric; there is no "concrete beats wildcard"
  rule beside it. What makes a running transcode win is that its announcement
  is seeded live while a standby wildcard is seeded high (`with_cost(1000)` is
  the existing convention for a production-cost seed). The seed has a floor: it
  MUST exceed the maximum accumulated topology cost a bounded hop list can
  reach (`MAX_HOPS` is 32 and the planned link costs are 1/3/5, so the ceiling is 160), or a nearby
  standby outranks a distant running transcode and the mesh starts a second
  encode of a stream it is already serving. That floor replaces the ad-hoc
  standby bias the moq.pro (downstream) transcode worker carries today, and it
  is the same stride discipline
  [pop-skipping](/quest/m2/pop-skipping/README.md) states for provider
  economics.
- **One cost varint, not the pair.** `Cost` is `{ warm, cold }` because a relay
  that is carrying a broadcast discounts the warm half. A wildcard carries
  nothing and can never be warm, so the two halves are provably equal and the
  message carries one value. `From<u64>` already means exactly this.
- **A wildcard is a capability, not an inventory.** It advertises what the
  sender could serve, never that a given path exists. Refusal is how a specific
  path is denied. This is why an over-claiming advertisement is not a defect:
  the catch-all `**` is legal, and answering "not that one" is the mechanism.
- **Containment against the publish scope is what authorization checks.** An
  advertised pattern MUST be contained by the sender's granted patterns (the
  matcher's containment check). This handles literal-headed and leading-star
  patterns identically and refuses any attempted widening rather than clamping
  it. Fleet-wide services use the cluster identity; a customer service may
  advertise only the exact set its own v1 grant contains.
- **Wildcards are visible to subscribers.** A subscriber sees every pattern
  matching under its scope, rebased by the matcher's exact set-valued operation,
  and duplicates combine into one. That is the point: it tells a client it may subscribe to
  matching paths, and its withdrawal tells the client the capability is gone.
  This is what makes a lazily-produced rendition discoverable without the
  composer waiting for an announcement that only demand would produce. The
  browser player currently enforces the opposite (`js/watch`'s
  `#isPathAnnounced` hides a catalog rendition with no exact-path
  announcement); [Demand](/quest/m2/wildcard/demand.md) makes a covering wildcard count as
  availability there.
- **Refusal is a typed stream reset, with no negative cache.** An advertiser
  resets a subscribe it will not serve, and the reset carries which KIND of
  refusal it is (`Error::to_code` already puts a typed code on the wire).

  A capacity refusal is unavoidable: an advertiser's capacity and a relay's view
  of it are separated by at least half a round trip, so a retraction and a
  request for the slot it just gave away WILL cross, at a rate of request rate
  times retraction rate times RTT. Only that code permits ONE re-resolution,
  with the refusing advertiser excluded from it. The exclusion is what makes the
  retry safe, NOT the retraction arriving first: the reset and the retraction
  travel independently, so re-resolution may pick another advertiser, and may
  equally find none and return unroutable. Both are correct outcomes.

  Every other refusal is terminal and propagates: a path no rule covers, an
  unauthorized one, one that does not exist. Scanning unserved paths therefore
  still costs one round trip per path, and this is not a fallback list: there
  is no walk down the candidates, only one re-resolution. Classification is
  EXPLICIT, per the repository's retry policy: an unrecognized or bare reset is
  permanent, so a new refusal mode surfaces instead of quietly joining a retry
  loop. No negative cache either way; rate limiting stays with the advertiser
  and the per-project auth gate.
- **A double claim is settled by the service's `Epoch` contract.** Two relays can
  hash one path to different workers before either concrete announcement
  propagates, and both concrete announcements land at the SAME literal path.
  Deterministic services such as transcode, and external processors whose
  configured workers are interchangeable, derive one epoch from the source
  generation so a relay may splice between them. Session-local services such as
  transcription allocate distinct globally ordered epochs, so the higher
  generation wins and consumers reset rather than splice. Wildcard routing does
  not invent a lease or choose between those media contracts. This is why
  [Epoch](/quest/m2/wildcard/epoch.md) is required by any service that can
  produce colliding concrete claims, including transcode, transcription, and
  external processors. A service that derives output identity from the source
  requires a resolved nonzero source epoch; zero is unspecified and must refuse
  derived work rather than splice across an ambiguous restart.
- **The spec home is moq-lite core, mirrored in moq-cluster**, following how
  route cost landed. moq-lite-06 is still WIP, so this goes into it rather than
  opening an 07.

### Where derived output lives

Suffix matching lets a contribution be published where it is addressed, a
descendant of its source. This is the moq.pro (downstream) deployment shape,
and it is what the suffix pattern form exists for:

```text
pid/foo.hang                     source
pid/foo.hang/catalog.pro         combined catalog, edge-composed
pid/foo.hang/transcode.pro       the transcode contribution
pid/foo.hang/transcribe.pro      the transcription contribution
```

The `.pro` segment suffix is both the routed pattern and the platform-output
marker: `**/transcode.pro` routes every project's transcode demand to the
worker pool, and a segment ending in `.pro` is the one predicate every source
rule matcher excludes, so platform output is never recursively transcoded or
recorded.

An earlier revision of this questline published contributions at mirrored paths
in reserved namespaces (`.transcode/<pid>/...`) hidden by a new origin-consumer
overlay, because prefix-only matching needs the variable part of a path
trailing. That overlay was not a view transform: `pid/foo` and
`.transcode/pid/foo` are separate tree leaves with separate broadcast fronts,
so it had to build a logical front across roots that re-owned route selection,
epoch identity, the split-horizon guard, and splicing. The suffix pattern
deletes all of it while keeping what the mirror bought:

- **The grant needs no transform.** The customer addresses
  `foo.hang/transcode.pro`, a descendant of `foo.hang`, so an existing grant
  covers it by ordinary segment-aware prefix. No companion-grant rule, no
  atomic `.pro/` scope, and nothing minted differently, which matters because
  customer-issued tokens are minted by integrations the platform does not
  control.
- **Metering is untouched.** The published path is rooted at `pid`, so the
  platform's egress metering sees the customer path with no special case at
  all.
- **The wildcard is fleet-wide.** The suffix is project-agnostic, so a worker
  advertises once for every project rather than once per project, which is what
  removes project discovery entirely.
- **Takeover is single-front.** A worker's concrete announcement lands at the
  literal path the wildcard served, so wildcard-versus-concrete and
  worker-versus-worker collisions are ordinary route selection plus `Epoch`
  splicing at one tree node, not a cross-root front.

What the mirror bought and this deliberately gives up: a customer holding
`publish: ["pid/"]` CAN publish `foo.hang/transcode.pro` themselves, competing
with or forging platform output. Both then resolve at one path, cost decides,
and a live customer broadcast beats the worker's standby seed. That is confined
to their own namespace, self-sabotage of their own catalog, never another
project's, and is cheaper to allow and document than a reserved-name registry
or a token transform. The mirror's SUBSCRIBE-only overlay asymmetry existed to
prevent exactly this and is gone with it.

The archive is the same shape at the source path itself: a recording IS the
broadcast, served from storage through the catch-all pattern. A wildcard names
no generation, so a client that must distinguish recording generations reads
the catalog's archive entry ([archive](/quest/m1/archive/README.md)) rather
than announce state.

## Quests

- [Draft](/quest/m2/wildcard/draft.md) - specify the wildcard advertisement in
  moq-lite and mirror it in the moq-cluster extension
- [Advertise](/quest/m2/wildcard/advertise.md) - moq-net encodes, forwards, and
  authorizes wildcard advertisements, without yet resolving one into a
  subscription
- [Resolve](/quest/m2/wildcard/resolve.md) - a relay resolves a subscribe or
  FETCH for an unannounced path against the best matching wildcard
- [Demand](/quest/m2/wildcard/demand.md) - the browser player subscribes to a
  catalog-referenced broadcast a wildcard covers, breaking the lazy-rendition
  deadlock
- [Epoch](/quest/m2/wildcard/epoch.md) - implement moq-lite-06's announcement
  `Epoch`, so two publishers colliding on one path resolve instead of both
  serving

## Related

- [path-patterns](/quest/m2/path-patterns/README.md) - owns the pattern dialect
  and the shared matcher advertisements reuse
- [archive](/quest/m1/archive/README.md) - an archive advertises the catch-all
  pattern, and its catalog names the generations a wildcard cannot
- [pop-skipping](/quest/m2/pop-skipping/README.md) - it owns the route cost and
  the rank hash this reuses
