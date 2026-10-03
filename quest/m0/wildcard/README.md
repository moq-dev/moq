# [L] Wildcard advertisements

## Goal

A service claims the prefix it could serve rather than enumerating every
broadcast under it; the client library filters that claim against the
pattern interest the caller asked for, so nothing on the wire spells a
wildcard. A claim is priced at what starting the work would cost. The longest
covering prefix wins first: a concrete announcement shadows a broader claim
regardless of cost, and prices compete only among claims of the same prefix.
A terminal concrete refusal does not fall through to a catch-all; its claim
must be withdrawn. Retracting a claim stops new work without shedding what is
already running.

Three workloads need this. A transcode
worker today announces a standby derivative for every matching live broadcast,
so announcements scale as workers times broadcasts; claiming one service
prefix, it advertises once for the whole fleet. A chat backend
cannot enumerate at all: rooms exist independently of any broadcast, and the
subtree pattern `<pid>/chat/**` expresses them. An archive serving recordings
over FETCH wants to say "if nobody is publishing this live, I have it", which
is the catch-all `**`, a claim about every path at once.

The cost of enumerating is real even though its last measurement is stale.
One announcement measured 8.8 KB per relay plus 4.3 KB per additional route
before prefix routes made a standby route a table entry. Whatever the current
number, every relay that hears an announcement pays it whether or not anything
there subscribes, so "workers times broadcasts" is that number multiplied
across the fleet in resident memory.

## Plan

Decided in [#3770](https://github.com/moq-dev/moq/pull/3770): publishing is
prefix-only on every wire and patterns never leave the token or the client library.
`dynamic(prefix, route)`
advertises a prefix; the catch-all claim is the root prefix, and the request
is the authority, so the advertise half of this questline is re-scoped to
prefix claims resolved against pattern interest. The three workloads above
still hold: the transcoder claims its service prefix
([Where derived output lives](#where-derived-output-lives)), and the archive
claims the root and refuses what it does not have.
Resolve and Demand are additive and land on main. Both are done on the line
branch (9d059b1b9 lists and demands covered renditions in the browser
player), so main no longer lists them; the line branch moves to
`quest/m0/wildcard/README` to match this path.

### What already exists, and what does not

Route cost already names this case: "The original publisher seeds it with its
production cost: zero for a live publish, something large for a standby that
would have to start working (a cold transcoder)"
(`drafts/draft-lcurley-moq-lite.md`). `moq_auth::Claims.publish` and
`origin::Producer` gained versioned patterns on the
[Auth](/quest/m1/auth/README.md) line, so tokens and filters reuse the
same matcher; advertisements stay prefixes. `Cost { warm, cold }`
(`rs/moq-net/src/model/origin.rs:426`) is the route cost since
[#2925](https://github.com/moq-dev/moq/pull/2925).

[moq#3225](https://github.com/moq-dev/moq/pull/3225) moved a long way toward
this, and #3770 settled the wire: an announcement carries a path prefix on
every protocol, and a consumer filters announced paths against its pattern
interest locally.

The routing table exists too. `Consumer::request_broadcast` resolves a local
broadcast first, then `best_server`: the longest covering prefix, filtered by
the requester's excluded hop, ordered by `route_order`
(`rs/moq-net/src/model/origin.rs:633`), served on demand by the session that
announced it and cached per prefix in `ServeState.served` (`:764`). That is the split-horizon-safe
lookup the old `origin::Dynamic` could not provide, and it is what
resolve extends rather than replaces.

Request resolution is prefix-only (`best_server` in
`rs/moq-net/src/model/origin.rs`) and stays that way. The pattern matcher
itself exists: `moq_net::{Pattern, Patterns, Segment}` and `Path.Pattern` /
`Path.Patterns` in `js/net/src/path.ts` own the shared matching, containment,
specificity, and rebasing tokens and filters reuse.

Content identity is the path: it names one broadcast whoever serves it, and
any covering route resumes a subscription from the first frame the subscriber
lacks (#4741). The `@<epoch>` segment (#4706) is what makes a restart a new
path. So this questline settles collisions with interchangeable output at one
path, not with a route identity or a generation field.

### Decisions

- **One prefix on the wire, one pattern in the token and the filter.** An
  advertisement is a path prefix; the pattern dialect from the
  [Auth](/quest/m1/auth/README.md) line is what tokens and the consume-side filter use, matched by the
  shared matcher, so nothing resembles a second grammar and nothing on the
  wire spells a wildcard.
- **Longest prefix wins, and its refusal is final.** This is the rule
  routing already follows: `best_server` filters to the longest covering prefix
  before it compares cost, and the lite draft says the same, matching
  longest-prefix-match wherever it appears. Advertisers of one prefix form one
  pool that cost and the request hash order. A terminal refusal from the
  winning tier IS the answer and never falls through to a shorter prefix, so a
  transcoder refusing a path does not leak the request to the archive's
  catch-all, and one unserved path still costs one round trip. The accepted
  consequence: an offline derivative is not reachable through the catch-all
  while a longer prefix covers it.
- **A claim is a POOL, not a competitor.** Several advertisers of one
  prefix is the normal state, not a hazard: every transcode worker claims the
  same prefix and takes a share. What distributes them is a
  deterministic hash of the REQUESTED path against each advertiser, so distinct
  paths spread rather than one advertiser winning the whole prefix.
  Distribution is the requirement, not any particular pair: a correct hash may
  legitimately rank the same advertiser first for two given paths, so what must
  hold is that a large path set spreads and that one path always resolves the
  same way. Cost orders the pool first, which keeps work local and makes a
  distant advertiser the overflow rather than an equal peer.
- **A claim is priced, not special-cased.** Within a tier, route selection
  stays one comparison on one metric. Concrete-versus-claim is not decided
  by price at all: a concrete announcement is the longest prefix, so "longest
  prefix wins" above already shadows every claim behind it at any cost. The
  accepted consequence follows from that rule's finality: a live session's
  concrete claim shadows a healthy pool even when its service is
  broken, its terminal refusal does not fall through, and the shadow lasts
  exactly as long as the claiming session that carries it.
  The seed still has a floor, because standby and running claims of the same
  prefix do meet: a standby concrete claim (`with_cost(1000)` is the
  existing per-broadcast convention) shares a tier with a running publisher's
  concrete announcement. The
  floor MUST exceed the deployment's enforced maximum charged-link count
  times its enforced maximum link cost (32 links at cost at most 5 gives
  a bound of 160, with producing origins seeded at 0), or a nearby standby outranks a distant running copy and the
  mesh starts a second encode of a stream it is already serving. That floor
  replaces the ad-hoc standby bias the moq.pro (downstream) transcode worker
  carries today.
- **A wildcard is a capability, not an inventory.** It advertises what the
  sender could serve, never that a given path exists. Refusal is how a specific
  path is denied. This is why an over-claiming advertisement is not a defect:
  the catch-all `**` is legal, and answering "not that one" is the mechanism.
- **Overlap with the publish scope is what authorization checks.** An
  advertised prefix MUST overlap the sender's granted patterns or it is
  refused. A prefix wider than the grant is accepted, but it only routes
  requests for paths the grant covers. Fleet-wide services use the cluster
  identity; a customer service serves only what its own v1 grant contains.
  Until [Advertise-only authorization](/quest/m2/processor/advertise-auth.md)
  lands, the publish scope stands in for advertising; a credential with its own
  advertise scope is checked against that instead.
- **Claims are visible to subscribers.** A subscriber sees every advertised
  prefix under its scope, filtered locally like any other announcement. That
  is the point: it tells a client it may subscribe to
  matching paths, and its withdrawal tells the client the capability is gone.
  This is what makes a lazily-produced rendition discoverable without the
  composer waiting for an announcement that only demand would produce. The
  browser player currently enforces the opposite (`js/watch`'s
  `#isPathAnnounced` hides a catalog rendition with no exact-path
  announcement); demand makes a covering wildcard count as
  availability there.
- **Every refusal is a terminal typed stream reset, with no negative cache.**
  An advertiser resets a subscribe it will not serve, and the reset carries
  which KIND of refusal it is (`Error::to_code` already puts a typed code on
  the wire). Every refusal propagates: a path no rule covers, an unauthorized
  one, one that does not exist, or one the advertiser has no capacity for.
  There is no re-resolution (decided 2026-10-03: NO_CAPACITY is removed, so
  no refusal is retryable). An advertiser sheds load by withdrawing or
  re-pricing its claim; a request that crosses the retraction is refused, and
  the client's ordinary resubscribe resolves again. Scanning unserved paths
  costs one round trip per path. No negative cache; rate limiting stays with
  the advertiser and the per-project auth gate.
- **A double claim is settled by interchangeable output, not by route
  identity or a lease.** Two relays can hash one path to different workers
  before either concrete announcement propagates, and both land at the SAME
  literal path. A path is one broadcast whoever serves it, so claim workers
  mirror the source's epoch and group numbers, publish a deterministic
  catalog, and start at group boundaries
  ([Transcoders start at group boundaries](/quest/m1/transcode-group-start.md)).
  Two workers at one path are then one broadcast, and a relay moving between
  them does so at a group boundary (decided 2026-10-03: #4741 drops
  first-hop identity, so routing can no longer tell two workers apart).
  Wildcard routing invents neither a lease nor a generation.
- **No reply Origin.** The lite-07 `Origin` field in SUBSCRIBE_OK and FETCH_OK,
  and the rule that a relay MUST NOT splice across differing Origins, are
  dropped (decided 2026-10-03: nothing needs them for correctness). The line
  branch carries code for Origin, `Identity`, and `Pin`, and #4050's
  NO_CAPACITY re-resolution; remove it when the branch next merges main.
- **Patterns are independent of clustering.** The `moq-pattern` crate owns
  the matching semantics tokens and filters share, with no draft of its own;
  no announce message carries a pattern on either protocol (AUTH grants on
  lite-06 do, per the [Auth](/quest/m1/auth/README.md) line). moq-cluster adds hop
  lists, costs, pool selection, and request resolution to prefix
  advertisements.

### Where derived output lives

A prefix claim needs the variable part of a path trailing, so a fleet-wide
service claims its own prefix and mirrors the source path beneath it
(`.pro/transcode/<pid>/foo.hang`, moq.pro's convention) rather than publishing
beneath the source. The source's catalog reaches the contribution through a
cross-broadcast reference.

The leading `.` is deliberate. Existing customers on moq-lite-06 or older must
never see `.pro/` broadcasts, which could confuse their business logic. Those
versions cannot opt into hidden routes, so the relay never announces them
there. Hidden routes are a moq-lite-07 feature, so the player's covering check
(Demand, done on the line branch) opts into them and sees a claim only when
lite-07 is negotiated. Finalizing lite-07 is a rollout condition, not a
blocker for this line (decided in the 2026-09-30 audit), since the check works
whenever lite-07 is negotiated. A customer who wants transcodes upgrades, or
subscribes to the explicit `.pro/<service>/...` path, which works on any
version. Grants and metering are the deployment's; moq.pro's are in its
[wildcard questline](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/wildcard/README.md).

The archive serves the source path itself: a recording IS the broadcast,
served from storage through the root claim, and a live publisher's concrete
announcement shadows it. A claim names no generation, so a client that must
distinguish recording generations reads the catalog's archive entry
([archive](/quest/m1/archive/README.md)) rather than announce state.

## Related

- [archive](/quest/m1/archive/README.md) - an archive claims the root, and its
  catalog names the generations a claim cannot
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - derived output
  mirrors the source path, `@<epoch>` segment included
- [Transcoders start at group boundaries](/quest/m1/transcode-group-start.md) -
  what makes two claim workers at one path interchangeable
- [Announcement shapes](/quest/m2/announce-shapes.md) - moq-lite-only exact,
  suffix, and prefix+suffix claims that survive relay hops
